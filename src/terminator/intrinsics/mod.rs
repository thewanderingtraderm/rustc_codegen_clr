use crate::{
    assembly::MethodCompileCtx,
    casts,
    operand::handle_operand,
    place::{place_adress, place_set},
};
use cilly::{
    call,
    cil_node::CILNode,
    cil_root::CILRoot,
    cilnode::MethodKind,
    conv_i16, conv_i32, conv_i64, conv_i8, conv_isize, conv_u16, conv_u32, conv_u64, conv_u8,
    conv_usize,
    tpe::int,
    v2::{ClassRef, Float, Int},
    Const, FieldDesc, IntoAsmIndex, MethodRef, Type,
};
use ints::{ctlz, rotate_left, rotate_right};
use rustc_middle::{
    mir::{Operand, Place},
    ty::{Instance, ParamEnv, Ty, UintTy},
};
use rustc_span::source_map::Spanned;
use saturating::{saturating_add, saturating_sub};
use type_info::{is_val_statically_known, size_of_val};
use utilis::{
    atomic_add, atomic_and, atomic_max, atomic_min, atomic_nand, atomic_or, atomic_xor,
    compare_bytes,
};
mod bswap;
mod floats;
mod interop;
mod ints;
mod saturating;
mod type_info;
mod utilis;
use floats::{fmaf32, fmaf64, powf32, powf64, powif32, powif64, roundf32, roundf64};
mod ptr;
use ptr::arith_offset;
mod mem;
use mem::{copy, raw_eq, write_bytes};
mod atomic;
mod tpe;
mod vtable;
pub fn breakpoint(args: &[Spanned<Operand<'_>>]) -> CILRoot {
    debug_assert_eq!(
        args.len(),
        0,
        "The intrinsic `breakpoint` MUST take in no arguments!"
    );
    CILRoot::Break
}
pub fn black_box<'tcx>(
    args: &[Spanned<Operand<'tcx>>],
    destination: &Place<'tcx>,
    call_instance: Instance<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILRoot {
    debug_assert_eq!(
        args.len(),
        1,
        "The intrinsic `black_box` MUST take in exactly 1 argument!"
    );
    let tpe = ctx.monomorphize(
        call_instance.args[0]
            .as_type()
            .expect("needs_drop works only on types!"),
    );
    let tpe = ctx.type_from_cache(tpe);
    if tpe == Type::Void {
        return CILRoot::Nop;
    }
    // assert_eq!(args.len(),1,"The intrinsic `unlikely` MUST take in exactly 1 argument!");
    place_set(destination, handle_operand(&args[0].node, ctx), ctx)
}

pub fn handle_intrinsic<'tcx>(
    fn_name: &str,
    args: &[Spanned<Operand<'tcx>>],
    destination: &Place<'tcx>,
    call_instance: Instance<'tcx>,
    span: rustc_span::Span,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> Vec<CILRoot> {
    match fn_name {
        "arith_offset" => vec![arith_offset(args, destination, call_instance, ctx)],
        "breakpoint" => vec![breakpoint(args)],
        "cold_path" | "assert_inhabited" | "assert_zero_valid" | "const_deallocate" => {
            vec![CILRoot::Nop]
        }
        "black_box" => vec![black_box(args, destination, call_instance, ctx)],
        "caller_location" => vec![caller_location(destination, ctx, span)],
        "compare_bytes" => vec![place_set(
            destination,
            compare_bytes(
                handle_operand(&args[0].node, ctx),
                handle_operand(&args[1].node, ctx),
                handle_operand(&args[2].node, ctx),
                ctx,
            ),
            ctx,
        )],
        "ctpop" => vec![ints::ctpop(args, destination, call_instance, ctx)],
        "bitreverse" => vec![ints::bitreverse(args, destination, ctx, call_instance)],
        "ctlz" | "ctlz_nonzero" => vec![ctlz(args, destination, call_instance, ctx)],
        "unlikely" | "likely" => {
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `{fn_name}` MUST take in exactly 1 argument!"
            );
            // assert_eq!(args.len(),1,"The intrinsic `unlikely` MUST take in exactly 1 argument!");
            vec![place_set(
                destination,
                handle_operand(&args[0].node, ctx),
                ctx,
            )]
        }
        "is_val_statically_known" => vec![is_val_statically_known(args, destination, ctx)],
        "needs_drop" => {
            debug_assert_eq!(
                args.len(),
                0,
                "The intrinsic `needs_drop` MUST take in exactly 0 argument!"
            );
            let tpe = ctx.monomorphize(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let needs_drop = tpe.needs_drop(
                ctx.tcx(),
                rustc_middle::ty::TypingEnv::fully_monomorphized(),
            );
            let needs_drop = i32::from(needs_drop);
            vec![place_set(
                destination,
                CILNode::V2(ctx.alloc_node(needs_drop)),
                ctx,
            )]
        }
        "fmaf32" => vec![fmaf32(args, destination, call_instance, ctx)],
        "fmaf64" => vec![fmaf64(args, destination, call_instance, ctx)],
        "raw_eq" => vec![raw_eq(args, destination, call_instance, ctx)],
        "bswap" => vec![bswap::bswap(args, destination, ctx)],
        "cttz" | "cttz_nonzero" => vec![ints::cttz(args, destination, ctx, call_instance)],
        "rotate_left" => vec![rotate_left(args, destination, ctx, call_instance)],
        "write_bytes" => vec![write_bytes(args, call_instance, ctx)],
        "copy" => vec![copy(args, call_instance, ctx)],
        "exact_div" => {
            debug_assert_eq!(
                args.len(),
                2,
                "The intrinsic `exact_div` MUST take in exactly 2 argument!"
            );

            vec![place_set(
                destination,
                crate::binop::binop(
                    rustc_middle::mir::BinOp::Div,
                    &args[0].node,
                    &args[1].node,
                    ctx,
                ),
                ctx,
            )]
        }
        "type_id" => vec![tpe::type_id(destination, call_instance, ctx)],
        "volatile_load" => vec![volitale_load(args, destination, ctx)],
        "volatile_store" => {
            let pointed_type = ctx.monomorphize(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let addr_calc = handle_operand(&args[0].node, ctx);
            let value_calc = handle_operand(&args[1].node, ctx);
            vec![CILRoot::Volatile(Box::new(crate::place::ptr_set_op(
                pointed_type.into(),
                ctx,
                addr_calc,
                value_calc,
            )))]
        }
        "atomic_load_unordered" => {
            // This is already implemented by default in .NET when volatile is used. TODO: ensure this is 100% right.
            //TODO:fix volitale prefix!
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `atomic_load_unordered` MUST take in exactly 1 argument!"
            );
            let arg = ctx.monomorphize(args[0].node.ty(ctx.body(), ctx.tcx()));
            let arg_ty = arg.builtin_deref(true).unwrap();
            let arg = handle_operand(&args[0].node, ctx);
            let ops = crate::place::deref_op(arg_ty.into(), ctx, arg);
            vec![place_set(destination, ops, ctx)]
        }
        "atomic_load_acquire" | "atomic_load_seqcst" => {
            //I am not sure this is implemented propely
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `atomic_load_acquire` MUST take in exactly 1 argument!"
            );
            let ops = handle_operand(&args[0].node, ctx);
            let arg = ctx.monomorphize(args[0].node.ty(ctx.body(), ctx.tcx()));
            let arg_ty = arg.builtin_deref(true).unwrap();

            let ops = crate::place::deref_op(arg_ty.into(), ctx, ops);
            vec![place_set(destination, ops, ctx)]
        }
        "atomic_store_relaxed"
        | "atomic_store_seqcst"
        | "atomic_store_release"
        | "atomic_store_unordered" => {
            // This is *propably* wrong :)
            debug_assert_eq!(
                args.len(),
                2,
                "The intrinsic `{fn_name}` MUST take in exactly 1 argument!"
            );
            let addr = handle_operand(&args[0].node, ctx);
            let val = handle_operand(&args[1].node, ctx);
            let arg_ty = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));

            vec![crate::place::ptr_set_op(arg_ty.into(), ctx, addr, val)]
        }
        "atomic_cxchgweak_acquire_acquire"
        | "atomic_cxchgweak_acquire_relaxed"
        | "atomic_cxchgweak_relaxed_relaxed"
        | "atomic_cxchgweak_relaxed_acquire"
        | "atomic_cxchgweak_seqcst_acquire"
        | "atomic_cxchgweak_seqcst_seqcst"
        | "atomic_cxchgweak_seqcst_relaxed"
        | "atomic_cxchg_acqrel_acquire"
        | "atomic_cxchg_acquire_seqcst"
        | "atomic_cxchg_release_relaxed"
        | "atomic_cxchg_relaxed_acquire"
        | "atomic_cxchg_acquire_relaxed"
        | "atomic_cxchg_relaxed_seqcst"
        | "atomic_cxchg_acquire_acquire"
        | "atomic_cxchg_release_acquire"
        | "atomic_cxchg_release_seqcst"
        | "atomic_cxchgweak_relaxed_seqcst"
        | "atomic_cxchgweak_acquire_seqcst"
        | "atomic_cxchgweak_release_relaxed"
        | "atomic_cxchgweak_release_acquire"
        | "atomic_cxchgweak_release_seqcst"
        | "atomic_cxchgweak_acqrel_relaxed"
        | "atomic_cxchgweak_acqrel_acquire"
        | "atomic_cxchgweak_acqrel_seqcst"
        | "atomic_cxchg_seqcst_seqcst"
        | "atomic_cxchg_seqcst_acquire"
        | "atomic_cxchg_seqcst_relaxed"
        | "atomic_cxchg_acqrel_relaxed"
        | "atomic_cxchg_relaxed_relaxed"
        | "atomic_cxchg_acqrel_seqcst" => vec![atomic::cxchg(args, destination, ctx)],
        "atomic_xsub_release"
        | "atomic_xsub_acqrel"
        | "atomic_xsub_acquire"
        | "atomic_xsub_relaxed"
        | "atomic_xsub_seqcst" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let sub_ammount = handle_operand(&args[1].node, ctx);
            // we sub by adding a negative number

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);
            match src_type {
                Type::Int(int) => {
                    let add_ammount = if int.is_signed() {
                        CILNode::Neg(Box::new(sub_ammount.clone()))
                    } else {
                        crate::casts::int_to_int(
                            Type::Int(int.as_signed()),
                            src_type,
                            CILNode::Neg(Box::new(crate::casts::int_to_int(
                                src_type,
                                Type::Int(int.as_signed()),
                                sub_ammount.clone(),
                                ctx,
                            ))),
                            ctx,
                        )
                    };
                    vec![place_set(
                        destination,
                        atomic_add(dst, add_ammount.clone(), src_type, ctx),
                        ctx,
                    )]
                }
                Type::Ptr(_) => {
                    let add_ammount = crate::casts::int_to_int(
                        Type::Int(Int::ISize),
                        Type::Int(Int::USize),
                        CILNode::Neg(Box::new(sub_ammount.cast_ptr(Type::Int(Int::ISize)))),
                        ctx,
                    );
                    vec![place_set(
                        destination,
                        atomic_add(dst, add_ammount.clone(), Type::Int(Int::USize), ctx)
                            .cast_ptr(src_type),
                        ctx,
                    )]
                }
                _ => panic!("{src_type:?} is not an int."),
            }
        }
        "atomic_or_seqcst" | "atomic_or_release" | "atomic_or_acqrel" | "atomic_or_acquire"
        | "atomic_or_relaxed" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let orand = handle_operand(&args[1].node, ctx);

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_or(dst, orand, src_type, ctx),
                ctx,
            )]
        }
        "atomic_xor_seqcst" | "atomic_xor_release" | "atomic_xor_acqrel" | "atomic_xor_acquire"
        | "atomic_xor_relaxed" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let xorand = handle_operand(&args[1].node, ctx);

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_xor(dst, xorand, src_type, ctx),
                ctx,
            )]
        }
        "atomic_and_seqcst" | "atomic_and_release" | "atomic_and_acqrel" | "atomic_and_acquire"
        | "atomic_and_relaxed" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let andand = handle_operand(&args[1].node, ctx);

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_and(dst, andand, src_type, ctx),
                ctx,
            )]
        }
        "atomic_nand_seqcst"
        | "atomic_nand_release"
        | "atomic_nand_acqrel"
        | "atomic_nand_acquire"
        | "atomic_nand_relaxed" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let andand = handle_operand(&args[1].node, ctx);

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_nand(dst, andand, src_type, ctx),
                ctx,
            )]
        }
        "atomic_fence_acquire"
        | "atomic_fence_seqcst"
        | "atomic_fence_release"
        | "atomic_fence_acqrel" => {
            let thread = ClassRef::thread(ctx);
            let fence = MethodRef::new(
                thread,
                ctx.alloc_string("MemoryBarrier"),
                ctx.sig([], Type::Void),
                MethodKind::Static,
                vec![].into(),
            );
            vec![CILRoot::Call {
                site: ctx.alloc_methodref(fence),
                args: [].into(),
            }]
        }
        "atomic_xadd_release"
        | "atomic_xadd_relaxed"
        | "atomic_xadd_seqcst"
        | "atomic_xadd_acqrel"
        | "atomic_xadd_acquire" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let add_ammount = handle_operand(&args[1].node, ctx);
            // we sub by adding a negative number

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_add(dst, add_ammount, src_type, ctx),
                ctx,
            )]
        }
        "atomic_umin_release"
        | "atomic_umin_relaxed"
        | "atomic_umin_seqcst"
        | "atomic_umin_acqrel"
        | "atomic_umin_acquire"
        | "atomic_min_release"
        | "atomic_min_relaxed"
        | "atomic_min_seqcst"
        | "atomic_min_acqrel"
        | "atomic_min_acquire" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let min_ammount = handle_operand(&args[1].node, ctx);
            // we sub by mining a negative number

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_min(dst, min_ammount, src_type, ctx),
                ctx,
            )]
        }
        "atomic_umax_release"
        | "atomic_umax_relaxed"
        | "atomic_umax_seqcst"
        | "atomic_umax_acqrel"
        | "atomic_umax_acquire"
        | "atomic_max_release"
        | "atomic_max_relaxed"
        | "atomic_max_seqcst"
        | "atomic_max_acqrel"
        | "atomic_max_acquire" => {
            // *T
            let dst = handle_operand(&args[0].node, ctx);
            // T
            let max_ammount = handle_operand(&args[1].node, ctx);
            // we sub by maxing a negative number

            let src_type = ctx.monomorphize(args[1].node.ty(ctx.body(), ctx.tcx()));
            let src_type = ctx.type_from_cache(src_type);

            vec![place_set(
                destination,
                atomic_max(dst, max_ammount, src_type, ctx),
                ctx,
            )]
        }
        "atomic_xchg_release"
        | "atomic_xchg_acquire"
        | "atomic_xchg_acqrel"
        | "atomic_xchg_relaxed"
        | "atomic_xchg_seqcst" => vec![atomic::xchg(args, destination, call_instance, ctx)],
        // TODO:Those are not stricly neccessary, but SHOULD be implemented at some point.
        "assert_inhabited" | "assert_zero_valid" | "const_deallocate" => vec![CILRoot::Nop],
        "ptr_offset_from_unsigned" => {
            vec![ptr::ptr_offset_from_unsigned(
                args,
                destination,
                call_instance,
                ctx,
            )]
        }
        "ptr_mask" => {
            debug_assert_eq!(
                args.len(),
                2,
                "The intrinsic `ptr_mask` MUST take in exactly 2 arguments!"
            );
            let tpe = ctx.monomorphize(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let tpe = ctx.type_from_cache(tpe);
            let tpe = ctx.nptr(tpe);

            vec![place_set(
                destination,
                CILNode::And(
                    Box::new(handle_operand(&args[0].node, ctx).cast_ptr(Type::Int(Int::USize))),
                    Box::new(handle_operand(&args[1].node, ctx)),
                )
                .cast_ptr(tpe),
                ctx,
            )]
        }
        "ptr_offset_from" => vec![ptr::ptr_offset_from(args, destination, call_instance, ctx)],
        "saturating_add" => vec![saturating_add(args, destination, ctx, call_instance)],
        "saturating_sub" => vec![saturating_sub(args, destination, ctx, call_instance)],
        "min_align_of_val" => {
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `min_align_of_val` MUST take in exactly 1 argument!"
            );
            let tpe = ctx.monomorphize(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let align = crate::utilis::align_of(tpe, ctx.tcx());
            vec![place_set(
                destination,
                conv_usize!(CILNode::V2(ctx.alloc_node(align))),
                ctx,
            )]
        }
        // .NET guarantess all loads are tear-free
        "atomic_load_relaxed" => {
            //I am not sure this is implemented propely
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `atomic_load_relaxed` MUST take in exactly 1 argument!"
            );
            let ops = handle_operand(&args[0].node, ctx);
            let arg = ctx.monomorphize(args[0].node.ty(ctx.body(), ctx.tcx()));
            let arg_ty = arg.builtin_deref(true).unwrap();

            let ops = crate::place::deref_op(arg_ty.into(), ctx, ops);
            vec![place_set(destination, ops, ctx)]
        }
        "sqrtf32" => {
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `sqrtf32` MUST take in exactly 1 argument!"
            );
            let sqrt = MethodRef::new(
                ClassRef::mathf(ctx),
                ctx.alloc_string("Sqrt"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            vec![place_set(
                destination,
                call!(
                    ctx.alloc_methodref(sqrt),
                    [handle_operand(&args[0].node, ctx)]
                ),
                ctx,
            )]
        }
        "carrying_mul_add" => {
            let wrapping = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let wint = wrapping
                .as_int()
                .expect("carrying_mul_add with a non-int type");

            let overflow = ctx.type_from_cache(
                call_instance.args[1]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let oint = overflow
                .as_int()
                .expect("carrying_mul_add with a non-int type");
            assert!(!matches!(oint, Int::U128 | Int::I128));
            let promoted = oint
                .promoted()
                .expect("Can't carrying_mul_add cause type is too large");
            let mul_a = casts::int_to_int(
                cilly::Type::Int(oint),
                cilly::Type::Int(promoted),
                handle_operand(&args[0].node, ctx),
                ctx,
            );
            let mul_b = casts::int_to_int(
                cilly::Type::Int(oint),
                cilly::Type::Int(promoted),
                handle_operand(&args[1].node, ctx),
                ctx,
            );
            let carry = casts::int_to_int(
                cilly::Type::Int(oint),
                cilly::Type::Int(promoted),
                handle_operand(&args[2].node, ctx),
                ctx,
            );
            let addend = casts::int_to_int(
                cilly::Type::Int(oint),
                cilly::Type::Int(promoted),
                handle_operand(&args[3].node, ctx),
                ctx,
            );
            let sum = if promoted.size() == Some(16) {
                let main_module = ctx.main_module();
                let op_mul = MethodRef::new(
                    *main_module,
                    ctx.alloc_string(format!("mul_{}", promoted.name())),
                    ctx.sig(
                        [Type::Int(promoted), Type::Int(promoted)],
                        Type::Int(promoted),
                    ),
                    MethodKind::Static,
                    vec![].into(),
                );
                let op_add = MethodRef::new(
                    *main_module,
                    ctx.alloc_string(format!("add_{}", promoted.name())),
                    ctx.sig(
                        [Type::Int(promoted), Type::Int(promoted)],
                        Type::Int(promoted),
                    ),
                    MethodKind::Static,
                    vec![].into(),
                );
                let op_mul = ctx.alloc_methodref(op_mul);
                let op_add = ctx.alloc_methodref(op_add);
                call!(
                    op_add,
                    [
                        call!(op_mul, [mul_a, mul_b]),
                        call!(op_add, [carry, addend])
                    ]
                )
            } else {
                (mul_a * mul_b) + carry + addend
            };
            let ovf = if promoted.size() == Some(16) {
                let main_module = ctx.main_module();
                let op_div = MethodRef::new(
                    *main_module,
                    ctx.alloc_string(format!("div_{}", promoted.name())),
                    ctx.sig(
                        [Type::Int(promoted), Type::Int(promoted)],
                        Type::Int(promoted),
                    ),
                    MethodKind::Static,
                    vec![].into(),
                );
                let op_div = ctx.alloc_methodref(op_div);
                call!(
                    op_div,
                    [
                        sum.clone(),
                        (casts::int_to_int(
                            cilly::Type::Int(Int::U128),
                            cilly::Type::Int(promoted),
                            CILNode::V2(ctx.alloc_node(1_u128 << (oint.size().unwrap_or(8) * 8))),
                            ctx,
                        ))
                    ]
                )
            } else {
                CILNode::DivUn(
                    Box::new(sum.clone()),
                    Box::new(casts::int_to_int(
                        cilly::Type::Int(Int::U64),
                        cilly::Type::Int(promoted),
                        CILNode::V2(ctx.alloc_node(1_u64 << (oint.size().unwrap_or(8) * 8))),
                        ctx,
                    )),
                )
            };

            let ovf =
                casts::int_to_int(cilly::Type::Int(promoted), cilly::Type::Int(wint), ovf, ctx);
            let wr =
                casts::int_to_int(cilly::Type::Int(promoted), cilly::Type::Int(oint), sum, ctx);
            let res_tpe = ctx
                .type_from_cache(destination.ty(ctx.body(), ctx.tcx()).ty)
                .as_class_ref()
                .unwrap();
            let dst = place_adress(destination, ctx);
            let item1 = ctx.alloc_string("Item1");
            let item2 = ctx.alloc_string("Item2");
            vec![
                CILRoot::SetField {
                    addr: Box::new(dst.clone()),
                    value: Box::new(wr),
                    desc: ctx.alloc_field(FieldDesc::new(res_tpe, item1, cilly::Type::Int(oint))),
                },
                CILRoot::SetField {
                    addr: Box::new(dst),
                    value: Box::new(ovf),
                    desc: ctx.alloc_field(FieldDesc::new(res_tpe, item2, cilly::Type::Int(wint))),
                },
            ]
        }
        "powif32" => vec![powif32(args, destination, call_instance, ctx)],
        "powif64" => vec![powif64(args, destination, call_instance, ctx)],
        "size_of_val" => vec![size_of_val(args, destination, ctx, call_instance)],
        "typed_swap_nonoverlapping" => {
            let pointed_ty = ctx.monomorphize(
                call_instance.args[0]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let tpe = ctx.monomorphize(pointed_ty);
            let tpe = ctx.type_from_cache(tpe);
            let void_ptr = ctx.nptr(Type::Void);
            let generic = MethodRef::new(
                *ctx.main_module(),
                ctx.alloc_string("swap_at_generic"),
                ctx.sig([void_ptr, void_ptr, Type::Int(Int::USize)], Type::Void),
                MethodKind::Static,
                vec![].into(),
            );
            vec![CILRoot::Call {
                site: ctx.alloc_methodref(generic),
                args: [
                    handle_operand(&args[0].node, ctx).cast_ptr(void_ptr),
                    handle_operand(&args[1].node, ctx).cast_ptr(void_ptr),
                    conv_usize!(CILNode::V2(ctx.size_of(tpe).into_idx(ctx))),
                ]
                .into(),
            }]
        }
        "type_name" => {
            let const_val = ctx
                .tcx()
                .const_eval_instance(
                    rustc_middle::ty::TypingEnv::fully_monomorphized(),
                    call_instance,
                    span,
                )
                .unwrap();
            vec![place_set(
                destination,
                crate::constant::load_const_value(const_val, Ty::new_static_str(ctx.tcx()), ctx),
                ctx,
            )]
        }
        "float_to_int_unchecked" => {
            let tpe = ctx.monomorphize(
                call_instance.args[1]
                    .as_type()
                    .expect("needs_drop works only on types!"),
            );
            let tpe = ctx.monomorphize(tpe);
            let tpe = ctx.type_from_cache(tpe);
            let input = handle_operand(&args[0].node, ctx);
            vec![place_set(
                destination,
                match tpe {
                    Type::Int(Int::U8) => conv_u8!(input),
                    Type::Int(Int::U16) => conv_u16!(input),
                    Type::Int(Int::U32) => conv_u32!(input),
                    Type::Int(Int::U64) => conv_u64!(input),
                    Type::Int(Int::USize) => conv_usize!(input),
                    Type::Int(Int::I8) => conv_i8!(input),
                    Type::Int(Int::I16) => conv_i16!(input),
                    Type::Int(Int::I32) => conv_i32!(input),
                    Type::Int(Int::I64) => conv_i64!(input),
                    Type::Int(Int::ISize) => conv_isize!(input),
                    _ => todo!("can't float_to_int_unchecked on {tpe:?}"),
                },
                ctx,
            )]
        }
        "fabsf32" => {
            let abs = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Abs"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(abs),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "fabsf64" => {
            let abs = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Abs"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );

            vec![place_set(
                destination,
                call!(
                    ctx.alloc_methodref(abs),
                    [handle_operand(&args[0].node, ctx),]
                ),
                ctx,
            )]
        }
        "expf32" => {
            let exp = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Exp"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(exp),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "expf64" => {
            let exp = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Exp"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(exp),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "logf32" => {
            let log = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Log"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(log),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "logf64" => {
            let log = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Log"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            vec![place_set(
                destination,
                call!(
                    ctx.alloc_methodref(log),
                    [handle_operand(&args[0].node, ctx),]
                ),
                ctx,
            )]
        }
        "log2f32" => {
            let log = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Log2"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(log),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "log2f64" => {
            let log = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Log2"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(log),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "log10f32" => {
            let log = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Log10"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(log),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "log10f64" => {
            let log = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Log10"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(log),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "powf32" => vec![powf32(args, destination, call_instance, ctx)],
        "powf64" => vec![powf64(args, destination, call_instance, ctx)],
        "copysignf32" => {
            let copy_sign = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("CopySign"),
                ctx.sig(
                    [Type::Float(Float::F32), Type::Float(Float::F32)],
                    Type::Float(Float::F32),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(copy_sign),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "copysignf64" => {
            let copy_sign = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("CopySign"),
                ctx.sig(
                    [Type::Float(Float::F64), Type::Float(Float::F64)],
                    Type::Float(Float::F64),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(copy_sign),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "sinf32" => {
            let sin = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Sin"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(sin),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "sinf64" => {
            let sin = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Sin"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(sin),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "cosf32" => {
            let cos = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Cos"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(cos),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "cosf64" => {
            let cos = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Cos"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(cos),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "exp2f32" => {
            let exp = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("Exp2"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(exp),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "exp2f64" => {
            let exp = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("Exp2"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(exp),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "truncf32" => {
            let trunc = MethodRef::new(
                ClassRef::mathf(ctx),
                ctx.alloc_string("Truncate"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(trunc),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "truncf64" => {
            let trunc = MethodRef::new(
                ClassRef::math(ctx),
                ctx.alloc_string("Truncate"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(trunc),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        // `roundf32` should be a differnt intrinsics, but it requires some .NET fuckery to implement(.NET enums are **wierd**)
        "nearbyintf32" | "rintf32" | "roundevenf32" => {
            let round = MethodRef::new(
                ClassRef::mathf(ctx),
                ctx.alloc_string("Round"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(round),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "roundf32" => vec![roundf32(args, destination, ctx)],
        "roundf64" => vec![roundf64(args, destination, ctx)],
        "nearbyintf64" | "rintf64" | "roundevenf64" => {
            let round = MethodRef::new(
                ClassRef::math(ctx),
                ctx.alloc_string("Round"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(round),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }

        "floorf32" => {
            let floor = MethodRef::new(
                ClassRef::mathf(ctx),
                ctx.alloc_string("Floor"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(floor),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "floorf64" => {
            let floor = MethodRef::new(
                ClassRef::math(ctx),
                ctx.alloc_string("Floor"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(floor),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "ceilf32" => {
            let ceil = MethodRef::new(
                ClassRef::mathf(ctx),
                ctx.alloc_string("Ceiling"),
                ctx.sig([Type::Float(Float::F32)], Type::Float(Float::F32)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(ceil),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "ceilf64" => {
            let ceil = MethodRef::new(
                ClassRef::math(ctx),
                ctx.alloc_string("Ceiling"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(ceil),
                [handle_operand(&args[0].node, ctx),]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "maxnumf64" => {
            let max = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("MaxNumber"),
                ctx.sig(
                    [Type::Float(Float::F64), Type::Float(Float::F64)],
                    Type::Float(Float::F64),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(max),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "maxnumf32" => {
            let max = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("MaxNumber"),
                ctx.sig(
                    [Type::Float(Float::F32), Type::Float(Float::F32)],
                    Type::Float(Float::F32),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(max),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "minnumf64" => {
            let min = MethodRef::new(
                ClassRef::double(ctx),
                ctx.alloc_string("MinNumber"),
                ctx.sig(
                    [Type::Float(Float::F64), Type::Float(Float::F64)],
                    Type::Float(Float::F64),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(min),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "minnumf32" => {
            let min = MethodRef::new(
                ClassRef::single(ctx),
                ctx.alloc_string("MinNumber"),
                ctx.sig(
                    [Type::Float(Float::F32), Type::Float(Float::F32)],
                    Type::Float(Float::F32),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            let value_calc = call!(
                ctx.alloc_methodref(min),
                [
                    handle_operand(&args[0].node, ctx),
                    handle_operand(&args[1].node, ctx),
                ]
            );
            vec![place_set(destination, value_calc, ctx)]
        }
        "variant_count" => {
            let const_val = ctx
                .tcx()
                .const_eval_instance(
                    rustc_middle::ty::TypingEnv::fully_monomorphized(),
                    call_instance,
                    span,
                )
                .unwrap();
            vec![place_set(
                destination,
                crate::constant::load_const_value(
                    const_val,
                    Ty::new_uint(ctx.tcx(), UintTy::Usize),
                    ctx,
                ),
                ctx,
            )]
        }
        "sqrtf64" => {
            debug_assert_eq!(
                args.len(),
                1,
                "The intrinsic `sqrtf64` MUST take in exactly 1 argument!"
            );
            let sqrt = MethodRef::new(
                ClassRef::math(ctx),
                ctx.alloc_string("Sqrt"),
                ctx.sig([Type::Float(Float::F64)], Type::Float(Float::F64)),
                MethodKind::Static,
                vec![].into(),
            );
            let ops = call!(
                ctx.alloc_methodref(sqrt),
                [handle_operand(&args[0].node, ctx)]
            );
            vec![place_set(destination, ops, ctx)]
        }
        "rotate_right" => vec![rotate_right(args, destination, ctx, call_instance)],
        "catch_unwind" => {
            debug_assert_eq!(
                args.len(),
                3,
                "The intrinsic `catch_unwind` MUST take in exactly 3 arguments!"
            );
            let try_fn = handle_operand(&args[0].node, ctx);
            let data_ptr = handle_operand(&args[1].node, ctx);
            let catch_fn = handle_operand(&args[2].node, ctx);
            let uint8_ptr = ctx.nptr(Type::Int(Int::U8));
            let try_ptr = ctx.sig([uint8_ptr], Type::Void);
            let catch_ptr = ctx.sig([uint8_ptr, uint8_ptr], Type::Void);
            let catch_unwind = MethodRef::new(
                *ctx.main_module(),
                ctx.alloc_string("catch_unwind"),
                ctx.sig(
                    [Type::FnPtr(try_ptr), uint8_ptr, Type::FnPtr(catch_ptr)],
                    Type::Int(Int::I32),
                ),
                MethodKind::Static,
                vec![].into(),
            );
            vec![place_set(
                destination,
                call!(
                    ctx.alloc_methodref(catch_unwind),
                    [try_fn, data_ptr, catch_fn]
                ),
                ctx,
            )]
        }
        "abort" => vec![CILRoot::throw("Called abort!", ctx)],
        "const_allocate" => vec![place_set(
            destination,
            CILNode::V2(ctx.alloc_node(Const::USize(0))),
            ctx,
        )],
        "vtable_size" => vec![vtable::vtable_size(args, destination, ctx)],
        "vtable_align" => vec![vtable::vtable_align(args, destination, ctx)],
        "simd_eq" => {
            let comparands = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let result = ctx.type_from_cache(
                call_instance.args[1]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_eq");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[comparands, comparands], result, name, ctx);
            vec![place_set(destination, call!(eq, [lhs, rhs]), ctx)]
        }
        "simd_or" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_or works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_or");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec, vec], vec, name, ctx);
            vec![place_set(destination, call!(eq, [lhs, rhs]), ctx)]
        }
        "simd_add" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_add works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_add");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec, vec], vec, name, ctx);
            vec![place_set(destination, call!(eq, [lhs, rhs]), ctx)]
        }
        "simd_and" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_and works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_and");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec, vec], vec, name, ctx);
            vec![place_set(destination, call!(eq, [lhs, rhs]), ctx)]
        }
        "simd_sub" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_sub works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_sub");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let sub = main_module.static_mref(&[vec, vec], vec, name, ctx);
            vec![place_set(destination, call!(sub, [lhs, rhs]), ctx)]
        }
        "simd_mul" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_mul works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let name = ctx.alloc_string("simd_mul");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let mul = main_module.static_mref(&[vec, vec], vec, name, ctx);
            vec![place_set(destination, call!(mul, [lhs, rhs]), ctx)]
        }
        "simd_fabs" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_fabs works only on types!"),
            );

            let lhs = handle_operand(&args[0].node, ctx);
            let name = ctx.alloc_string("simd_abs");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let abs = main_module.static_mref(&[vec], vec, name, ctx);
            vec![place_set(destination, call!(abs, [lhs,]), ctx)]
        }
        "simd_bitmask" => {
            let vec: Type = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_bitmask works only on types!"),
            );
            let int = ctx.type_from_cache(
                call_instance.args[1]
                    .as_type()
                    .expect("simd_bitmask works only on types!"),
            );
            let int = int
                .as_int()
                .expect("simd_bitmask only currently supports bitpacking ints.");
            let lhs = handle_operand(&args[0].node, ctx);
            let name = ctx.alloc_string("simd_get_most_significant_bits");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let most_significant_bits = main_module.static_mref(&[vec], Type::Int(int), name, ctx);
            vec![place_set(
                destination,
                call!(most_significant_bits, [lhs]),
                ctx,
            )]
        }
        "simd_neg" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_neg works only on types!"),
            );
            let val = handle_operand(&args[0].node, ctx);
            let name = ctx.alloc_string("simd_neg");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec], vec, name, ctx);
            vec![place_set(destination, call!(eq, [val]), ctx)]
        }
        "simd_shuffle" => {
            let t_type = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let u_type = ctx.type_from_cache(
                call_instance.args[1]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let v_type = ctx.type_from_cache(
                call_instance.args[2]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let x = handle_operand(&args[0].node, ctx);
            let y = handle_operand(&args[1].node, ctx);
            // When the two vectors provided to simd shuffles are always the same, and have a length of 1(are scalar), the shuffle is equivalent to creating a vector [scalar,scalar].
            if x == y && matches!(t_type, Type::Int(_) | Type::Float(_)) {
                let name = ctx.alloc_string("simd_vec_from_val");
                let main_module = ctx.main_module();
                let main_module = ctx[*main_module].clone();
                let shuffle = main_module.static_mref(&[t_type], v_type, name, ctx);
                // SANITY: for this optimzation to work, the u(index vector) and v(result vector) both have to have be vectors.
                let (_u_type, _v_type) = (
                    u_type.as_simdvector().unwrap(),
                    v_type.as_simdvector().unwrap(),
                );
                return vec![place_set(destination, call!(shuffle, [x]), ctx)];
            }
            let idx = handle_operand(&args[2].node, ctx);
            let name = ctx.alloc_string("simd_shuffle");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let shuffle = main_module.static_mref(&[t_type, t_type, u_type], v_type, name, ctx);
            vec![place_set(destination, call!(shuffle, [x, y, idx]), ctx)]
        }
        "simd_ne" => {
            let comparands = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let result = ctx.type_from_cache(
                call_instance.args[1]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let lhs = handle_operand(&args[0].node, ctx);
            let rhs = handle_operand(&args[1].node, ctx);
            let eq = ctx.alloc_string("simd_eq");
            let ones_compliment = ctx.alloc_string("simd_ones_compliment");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[comparands, comparands], result, eq, ctx);
            let eq = call!(eq, [lhs, rhs]);
            let ones_compliment = main_module.static_mref(&[result], result, ones_compliment, ctx);
            let ne = call!(ones_compliment, [eq]);
            vec![place_set(destination, ne, ctx)]
        }
        "simd_reduce_any" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let x = handle_operand(&args[0].node, ctx);
            let simd_eq = ctx.alloc_string("simd_eq_any");
            let allset = ctx.alloc_string("simd_allset");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec, vec], Type::Bool, simd_eq, ctx);
            let allset = main_module.static_mref(&[], vec, allset, ctx);
            let allset = call!(allset, []);
            vec![place_set(destination, call!(eq, [x, allset]), ctx)]
        }
        "select_unpredictable" => {
            let tpe = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let cond = handle_operand(&args[0].node, ctx);
            let true_val = handle_operand(&args[1].node, ctx);
            let false_val = handle_operand(&args[2].node, ctx);

            let select = CILNode::select(tpe, true_val, false_val, cond, ctx);
            vec![place_set(destination, select, ctx)]
        }
        "simd_reduce_all" => {
            let vec = ctx.type_from_cache(
                call_instance.args[0]
                    .as_type()
                    .expect("simd_eq works only on types!"),
            );
            let x = handle_operand(&args[0].node, ctx);
            let simd_eq = ctx.alloc_string("simd_eq_all");
            let allset = ctx.alloc_string("simd_allset");
            let main_module = ctx.main_module();
            let main_module = ctx[*main_module].clone();
            let eq = main_module.static_mref(&[vec, vec], Type::Bool, simd_eq, ctx);
            let allset = main_module.static_mref(&[], vec, allset, ctx);
            let allset = call!(allset, []);
            vec![place_set(destination, call!(eq, [x, allset]), ctx)]
        }
        _ => intrinsic_slow(fn_name, args, destination, ctx, call_instance, span),
    }
}
fn intrinsic_slow<'tcx>(
    fn_name: &str,
    args: &[Spanned<Operand<'tcx>>],
    destination: &Place<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
    call_instance: Instance<'tcx>,
    span: rustc_span::Span,
) -> Vec<CILRoot> {
    // Then, demangle the type name, converting it to a Rust-style one (eg. `core::option::Option::h8zc8s`)
    let demangled = rustc_demangle::demangle(fn_name);
    // Using formating preserves the generic hash.
    let demangled = format!("{demangled:#}");
    if demangled == fn_name {
        todo!("Unhandled intrinsic {fn_name}. demangled:{demangled}")
    } else {
        assert!(demangled.contains("::"));
        let striped = demangled.split("::").last().unwrap();
        handle_intrinsic(striped, args, destination, call_instance, span, ctx)
    }
}
fn volitale_load<'tcx>(
    args: &[Spanned<Operand<'tcx>>],
    destination: &Place<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILRoot {
    //TODO:fix volitale prefix!
    debug_assert_eq!(
        args.len(),
        1,
        "The intrinsic `volatile_load` MUST take in exactly 1 argument!"
    );
    let arg = ctx.monomorphize(args[0].node.ty(ctx.body(), ctx.tcx()));
    let arg_ty = arg.builtin_deref(true).unwrap();
    let arg = handle_operand(&args[0].node, ctx);
    let ops = CILNode::Volatile(Box::new(crate::place::deref_op(arg_ty.into(), ctx, arg)));
    place_set(destination, ops, ctx)
}
fn caller_location<'tcx>(
    destination: &Place<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
    span: rustc_span::Span,
) -> CILRoot {
    let caller_loc = ctx.tcx().span_as_caller_location(span);
    let caller_loc_ty = ctx.tcx().caller_location_ty();
    crate::place::place_set(
        destination,
        crate::constant::load_const_value(caller_loc, caller_loc_ty, ctx),
        ctx,
    )
}
