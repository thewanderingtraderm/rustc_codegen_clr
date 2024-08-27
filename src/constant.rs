use core::f16;

use crate::{assembly::MethodCompileCtx, r#type::get_type};

use cilly::{
    call,
    call_site::CallSite,
    cil_node::{CILNode, CallOpArgs},
    cil_root::CILRoot,
    conv_u64, conv_usize,
    field_desc::FieldDescriptor,
    ldc_u64,
    static_field_desc::StaticFieldDescriptor,
    v2::{
        hashable::{HashableF32, HashableF64},
        Assembly, ClassRef, Float, Int,
    },
    FnSig, Type,
};

use rustc_middle::{
    mir::{
        interpret::{AllocId, GlobalAlloc, Scalar},
        ConstOperand, ConstValue,
    },
    ty::{FloatTy, IntTy, ParamEnv, Ty, TyKind, UintTy},
};
pub fn handle_constant<'tcx>(
    constant_op: &ConstOperand<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILNode {
    let constant = constant_op.const_;
    let constant = ctx.monomorphize(constant);
    let evaluated = constant
        .eval(ctx.tcx(), ParamEnv::reveal_all(), constant_op.span)
        .expect("Could not evaluate constant!");
    load_const_value(evaluated, constant.ty(), ctx)
}
/// Returns the ops neceasry to create constant value of type `ty` with byte values matching the ones in the allocation
fn create_const_from_data<'tcx>(
    ty: Ty<'tcx>,
    alloc_id: AllocId,
    offset_bytes: u64,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILNode {
    let _ = offset_bytes;
    let ptr = CILNode::LoadGlobalAllocPtr {
        alloc_id: alloc_id.0.into(),
    };
    let ty = ctx.monomorphize(ty);
    let tpe = ctx.type_from_cache(ty);
    let tpe_ptr = ctx.asm_mut().nptr(tpe);
    crate::place::deref_op(ty.into(), ctx, ptr.cast_ptr(tpe_ptr))
}

pub(crate) fn load_const_value<'tcx>(
    const_val: ConstValue<'tcx>,
    const_ty: Ty<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILNode {
    match const_val {
        ConstValue::Scalar(scalar) => load_const_scalar(scalar, const_ty, ctx),
        ConstValue::ZeroSized => {
            let tpe = ctx.monomorphize(const_ty);
            assert!(
                crate::utilis::is_zst(tpe, ctx.tcx()),
                "Zero sized const with a non-zero size. It is {tpe:?}"
            );
            let tpe = ctx.type_from_cache(tpe);
            CILNode::TemporaryLocal(Box::new((tpe, [].into(), CILNode::LoadTMPLocal)))
        }
        ConstValue::Slice { data, meta } => {
            let slice_type = get_type(const_ty, ctx);
            let slice_dotnet = slice_type.as_class_ref().expect("Slice type invalid!");
            let metadata_field = FieldDescriptor::new(
                slice_dotnet.clone(),
                cilly::v2::Type::Int(Int::USize),
                crate::METADATA.into(),
            );
            let ptr_field = FieldDescriptor::new(
                slice_dotnet,
                ctx.asm_mut().nptr(cilly::v2::Type::Void),
                crate::DATA_PTR.into(),
            );
            // TODO: find a better way to get an alloc_id. This is likely to be incoreect.
            let alloc_id = ctx.tcx().reserve_and_set_memory_alloc(data);
            let alloc_id: u64 = crate::utilis::alloc_id_to_u64(alloc_id);
            let slice_type = ctx.type_from_cache(const_ty);
            CILNode::TemporaryLocal(Box::new((
                slice_type,
                [
                    CILRoot::SetField {
                        addr: Box::new(CILNode::LoadAddresOfTMPLocal),
                        value: Box::new(conv_usize!(ldc_u64!(meta))),
                        desc: Box::new(metadata_field),
                    },
                    CILRoot::SetField {
                        addr: Box::new(CILNode::LoadAddresOfTMPLocal),
                        value: Box::new(
                            CILNode::LoadGlobalAllocPtr { alloc_id }
                                .cast_ptr(ctx.asm_mut().nptr(Type::Void)),
                        ),
                        desc: Box::new(ptr_field),
                    },
                ]
                .into(),
                CILNode::LoadTMPLocal,
            )))
        }
        ConstValue::Indirect { alloc_id, offset } => {
            create_const_from_data(const_ty, alloc_id, offset.bytes(), ctx)
            //todo!("Can't handle by-ref allocation {alloc_id:?} {offset:?}")
        } //_ => todo!("Unhandled const value {const_val:?} of type {const_ty:?}"),
    }
}
fn load_scalar_ptr(
    ctx: &mut MethodCompileCtx<'_, '_>,
    ptr: rustc_middle::mir::interpret::Pointer,
) -> CILNode {
    let (alloc_id, offset) = ptr.into_parts();
    let global_alloc = ctx.tcx().global_alloc(alloc_id.alloc_id());
    let u8_ptr = ctx.asm_mut().nptr(Type::Int(Int::U8));
    match global_alloc {
        GlobalAlloc::Static(def_id) => {
            assert!(ctx.tcx().is_static(def_id));
            assert_eq!(offset.bytes(), 0);
            let name = ctx
                .tcx()
                .opt_item_name(def_id)
                .expect("Static without name")
                .to_string();
            /* */
            if name == "__rust_alloc_error_handler_should_panic"
                || name == "__rust_no_alloc_shim_is_unstable"
            {
                return CILNode::TemporaryLocal(Box::new((
                    Type::Int(Int::U8),
                    [CILRoot::SetTMPLocal {
                        value: CILNode::LDStaticField(
                            StaticFieldDescriptor::new(
                                None,
                                Type::Int(Int::U8),
                                name.clone().into(),
                            )
                            .into(),
                        ),
                    }]
                    .into(),
                    CILNode::LoadAddresOfTMPLocal,
                )));
            }
            if name == "environ" {
                return CILNode::TemporaryLocal(Box::new((
                    ctx.asm_mut().nptr(u8_ptr),
                    [CILRoot::SetTMPLocal {
                        value: CILNode::Call(Box::new(CallOpArgs {
                            args: Box::new([]),
                            site: Box::new(CallSite::new(
                                None,
                                "get_environ".into(),
                                FnSig::new(&[], ctx.asm_mut().nptr(u8_ptr)),
                                true,
                            )),
                        })),
                    }]
                    .into(),
                    CILNode::LoadAddresOfTMPLocal,
                )));
            }
            let attrs = ctx.tcx().codegen_fn_attrs(def_id);

            if let Some(_) = attrs.import_linkage {
                // TODO: this could cause issues if the pointer to the static is not imediatly dereferenced.
                let site = get_fn_from_static_name(&name, ctx);
                return CILNode::TemporaryLocal(Box::new((
                    Type::FnPtr(
                        ctx.asm_mut()
                            .alloc_sig(cilly::v2::FnSig::from_v1(site.signature())),
                    ),
                    [CILRoot::SetTMPLocal {
                        value: CILNode::LDFtn(Box::new(site)),
                    }]
                    .into(),
                    CILNode::LoadAddresOfTMPLocal,
                )));
            }
            if let Some(section) = attrs.link_section {
                panic!("static {name} requires special linkage in section {section:?}");
            }
            let alloc = ctx
                .tcx()
                .eval_static_initializer(def_id)
                .expect("No initializer??");
            //def_id.ty();
            let _memory = ctx.tcx().reserve_and_set_memory_alloc(alloc);
            let alloc_id = crate::utilis::alloc_id_to_u64(alloc_id.alloc_id());
            CILNode::LoadGlobalAllocPtr { alloc_id }
        }
        GlobalAlloc::Memory(_const_allocation) => {
            if offset.bytes() != 0 {
                CILNode::Add(
                    CILNode::LoadGlobalAllocPtr {
                        alloc_id: alloc_id.alloc_id().0.into(),
                    }
                    .into(),
                    CILNode::ZeroExtendToUSize(CILNode::LdcU64(offset.bytes()).into()).into(),
                )
            } else {
                CILNode::LoadGlobalAllocPtr {
                    alloc_id: alloc_id.alloc_id().0.into(),
                }
            }
        }
        GlobalAlloc::Function {
            instance: finstance,
        } => {
            assert_eq!(offset.bytes(), 0);
            // If it is a function, patch its pointer up.
            let call_info = crate::call_info::CallInfo::sig_from_instance_(finstance, ctx);
            let function_name = crate::utilis::function_name(ctx.tcx().symbol_name(finstance));
            return CILNode::LDFtn(
                CallSite::new(None, function_name, call_info.sig().clone(), true).into(),
            );
        }
        GlobalAlloc::VTable(..) => todo!("Unhandled global alloc {global_alloc:?}"),
    }
    //panic!("alloc_id:{alloc_id:?}")
}
fn load_const_scalar<'tcx>(
    scalar: Scalar,
    scalar_type: Ty<'tcx>,
    ctx: &mut MethodCompileCtx<'tcx, '_>,
) -> CILNode {
    let scalar_ty = ctx.monomorphize(scalar_type);

    let scalar_type = ctx.type_from_cache(scalar_ty);
    let scalar_u128 = match scalar {
        Scalar::Int(scalar_int) => scalar_int.to_uint(scalar.size()),
        Scalar::Ptr(ptr, _size) => {
            if matches!(scalar_type, Type::Ptr(_) | Type::FnPtr(_)) {
                return load_scalar_ptr(ctx, ptr).cast_ptr(scalar_type);
            }

            return CILNode::LdObj {
                obj: Box::new(scalar_type.clone()),
                ptr: Box::new(CILNode::TemporaryLocal(Box::new((
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    [CILRoot::SetTMPLocal {
                        value: load_scalar_ptr(ctx, ptr),
                    }]
                    .into(),
                    CILNode::LoadAddresOfTMPLocal.cast_ptr(ctx.asm_mut().nptr(scalar_type)),
                )))),
            };
        }
    };

    match scalar_ty.kind() {
        TyKind::Int(int_type) => load_const_int(scalar_u128, *int_type, ctx.asm_mut()),
        TyKind::Uint(uint_type) => load_const_uint(scalar_u128, *uint_type, ctx.asm_mut()),
        TyKind::Float(ftype) => load_const_float(scalar_u128, *ftype, ctx.asm_mut()),
        TyKind::Bool => {
            if scalar_u128 == 0 {
                CILNode::LdFalse
            } else {
                CILNode::LdTrue
            }
        }
        TyKind::RawPtr(_, _) => conv_usize!(ldc_u64!(
            u64::try_from(scalar_u128).expect("pointers must be smaller than 2^64")
        ))
        .cast_ptr(scalar_type),
        TyKind::Tuple(elements) => {
            if elements.is_empty() {
                CILNode::TemporaryLocal(Box::new((
                    ctx.asm_mut().nptr(scalar_type.clone()),
                    [].into(),
                    CILNode::LdObj {
                        ptr: CILNode::LoadTMPLocal.into(),
                        obj: Type::Void.into(),
                    },
                )))
            } else {
                CILNode::LdObj {
                    ptr: Box::new(
                        CILNode::PointerToConstValue(Box::new(scalar_u128))
                            .cast_ptr(ctx.asm_mut().nptr(scalar_type.clone())),
                    ),
                    obj: scalar_type.into(),
                }
            }
        }
        TyKind::Adt(_, _) | TyKind::Closure(_, _) => CILNode::LdObj {
            ptr: Box::new(
                CILNode::PointerToConstValue(Box::new(scalar_u128))
                    .cast_ptr(ctx.asm_mut().nptr(scalar_type.clone())),
            ),
            obj: scalar_type.into(),
        },
        TyKind::Char => CILNode::LdcU32(u32::try_from(scalar_u128).unwrap()),
        _ => todo!("Can't load scalar constants of type {scalar_ty:?}!"),
    }
}
fn load_const_float(value: u128, float_type: FloatTy, asm: &mut Assembly) -> CILNode {
    match float_type {
        FloatTy::F16 => {
            #[cfg(not(target_family = "windows"))]
            {
                call!(
                    CallSite::new_extern(
                        ClassRef::half(asm),
                        "op_Explicit".into(),
                        FnSig::new(&[Type::Float(Float::F32)], Type::Float(Float::F16)),
                        true
                    ),
                    [CILNode::LdcF32(HashableF32(
                        (f16::from_ne_bytes((u16::try_from(value).unwrap()).to_ne_bytes())) as f32
                    ),)]
                )
            }
            #[cfg(target_family = "windows")]
            {
                todo!("building a program using 16 bit floats is not supported on windwows yet")
                // TODO: check if this still causes a linker error on windows
            }
        }
        FloatTy::F32 => {
            let value = f32::from_ne_bytes((u32::try_from(value).unwrap()).to_ne_bytes());
            CILNode::LdcF32(HashableF32(value))
        }
        FloatTy::F64 => {
            let value = f64::from_ne_bytes((u64::try_from(value).unwrap()).to_ne_bytes());
            CILNode::LdcF64(HashableF64(value))
        }
        FloatTy::F128 => {
            // Int128 is used to emulate f128
            let low = u128_low_u64(value);
            let high = (value >> 64) as u64;
            let ctor_sig = FnSig::new(
                &[
                    asm.nref(Type::Float(Float::F128).into()),
                    Type::Int(Int::U64),
                    Type::Int(Int::U64),
                ],
                Type::Void,
            );
            CILNode::TemporaryLocal(Box::new((
                Type::Int(Int::I128),
                Box::new([CILRoot::SetTMPLocal {
                    value: CILNode::NewObj(Box::new(CallOpArgs {
                        site: CallSite::boxed(
                            Some(ClassRef::int_128(asm)),
                            ".ctor".into(),
                            ctor_sig,
                            false,
                        ),
                        args: [conv_u64!(ldc_u64!(high)), conv_u64!(ldc_u64!(low))].into(),
                    })),
                }]),
                CILNode::LdObj {
                    ptr: Box::new(
                        CILNode::LoadAddresOfTMPLocal.cast_ptr(asm.nptr(Type::Float(Float::F128))),
                    ),
                    obj: Box::new(Type::Float(Float::F128)),
                },
            )))
        }
    }
}
pub fn load_const_int(value: u128, int_type: IntTy, asm: &mut Assembly) -> CILNode {
    match int_type {
        IntTy::I8 => {
            let value = i8::from_ne_bytes([u8::try_from(value).unwrap()]);
            CILNode::LdcI8(value)
        }
        IntTy::I16 => {
            let value = i16::from_ne_bytes((u16::try_from(value).unwrap()).to_ne_bytes());
            CILNode::LdcI16(value)
        }
        IntTy::I32 => CILNode::LdcI32(i32::from_ne_bytes(
            (u32::try_from(value).unwrap()).to_ne_bytes(),
        )),
        IntTy::I64 => CILNode::SignExtendToI64(
            CILNode::LdcI64(i64::from_ne_bytes(
                (u64::try_from(value).unwrap()).to_ne_bytes(),
            ))
            .into(),
        ),
        IntTy::Isize => CILNode::SignExtendToISize(
            CILNode::LdcI64(i64::from_ne_bytes(
                (u64::try_from(value).unwrap()).to_ne_bytes(),
            ))
            .into(),
        ),
        IntTy::I128 => {
            let low = u128_low_u64(value);
            let high = (value >> 64) as u64;
            let ctor_sig = FnSig::new(
                &[
                    asm.nref(Type::Int(Int::I128)),
                    Type::Int(Int::U64),
                    Type::Int(Int::U64),
                ],
                Type::Void,
            );
            CILNode::NewObj(Box::new(CallOpArgs {
                site: CallSite::boxed(
                    Some(ClassRef::int_128(asm)),
                    ".ctor".into(),
                    ctor_sig,
                    false,
                ),
                args: [conv_u64!(ldc_u64!(high)), conv_u64!(ldc_u64!(low))].into(),
            }))
        }
    }
}
pub fn load_const_uint(value: u128, int_type: UintTy, asm: &mut Assembly) -> CILNode {
    match int_type {
        UintTy::U8 => {
            let value = u8::try_from(value).unwrap();
            CILNode::ConvU8(CILNode::LdcU32(u32::from(value)).into())
        }
        UintTy::U16 => {
            let value = u16::try_from(value).unwrap();
            CILNode::ConvU16(CILNode::LdcU32(u32::from(value)).into())
        }
        UintTy::U32 => CILNode::ConvU32(CILNode::LdcU32(u32::try_from(value).unwrap()).into()),
        UintTy::U64 => {
            CILNode::ZeroExtendToU64(CILNode::LdcU64(u64::try_from(value).unwrap()).into())
        }
        UintTy::Usize => {
            CILNode::ZeroExtendToUSize(CILNode::LdcU64(u64::try_from(value).unwrap()).into())
        }
        UintTy::U128 => {
            let low = u128_low_u64(value);
            let high = (value >> 64) as u64;
            let ctor_sig = FnSig::new(
                &[
                    asm.nref(Type::Int(Int::U128).into()),
                    Type::Int(Int::U64),
                    Type::Int(Int::U64),
                ],
                Type::Void,
            );
            CILNode::NewObj(Box::new(CallOpArgs {
                site: CallSite::boxed(
                    Some(ClassRef::uint_128(asm)),
                    ".ctor".into(),
                    ctor_sig,
                    false,
                ),
                args: [conv_u64!(ldc_u64!(high)), conv_u64!(ldc_u64!(low))].into(),
            }))
        }
    }
}
fn u128_low_u64(value: u128) -> u64 {
    u64::try_from(value & u128::from(u64::MAX)).expect("trucating cast error")
}
fn get_fn_from_static_name(name: &str, ctx: &mut MethodCompileCtx<'_, '_>) -> CallSite {
    let int8_ptr = ctx.asm_mut().nptr(Type::Int(Int::I8));
    let void_ptr = ctx.asm_mut().nptr(Type::Void);
    match name {
        "statx" => CallSite::builtin(
            "statx".into(),
            FnSig::new(
                &[
                    Type::Int(Int::I32),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    Type::Int(Int::I32),
                    Type::Int(Int::U32),
                    void_ptr,
                ],
                Type::Int(Int::I32),
            ),
            true,
        ),
        "getrandom" => CallSite::builtin(
            "getrandom".into(),
            FnSig::new(
                &[
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    Type::Int(Int::USize),
                    Type::Int(Int::U32),
                ],
                Type::Int(Int::USize),
            ),
            true,
        ),
        "posix_spawn" => CallSite::builtin(
            "posix_spawn".into(),
            FnSig::new(
                &[
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                ],
                Type::Int(Int::I32),
            ),
            true,
        ),
        "posix_spawn_file_actions_addchdir_np" => CallSite::builtin(
            "posix_spawn_file_actions_addchdir_np".into(),
            FnSig::new(
                &[
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                    ctx.asm_mut().nptr(Type::Int(Int::U8)),
                ],
                Type::Int(Int::I32),
            ),
            true,
        ),
        "__dso_handle" => {
            CallSite::builtin("__dso_handle".into(), FnSig::new(&[], Type::Void), true)
        }
        "__cxa_thread_atexit_impl" => CallSite::builtin(
            "__cxa_thread_atexit_impl".into(),
            FnSig::new(
                &[
                    Type::FnPtr(ctx.asm_mut().sig([void_ptr], Type::Void)),
                    void_ptr,
                    void_ptr,
                ],
                Type::Void,
            ),
            true,
        ),
        "copy_file_range" => CallSite::builtin(
            "copy_file_range".into(),
            FnSig::new(
                &[
                    Type::Int(Int::I32),
                    ctx.asm_mut().nptr(Type::Int(Int::I64)),
                    Type::Int(Int::I32),
                    ctx.asm_mut().nptr(Type::Int(Int::I64)),
                    Type::Int(Int::ISize),
                    Type::Int(Int::U32),
                ],
                Type::Int(Int::ISize),
            ),
            true,
        ),
        "pidfd_spawnp" => CallSite::builtin(
            "pidfd_spawnp".into(),
            FnSig::new(
                &[
                    ctx.asm_mut().nptr(Type::Int(Int::I32)),
                    ctx.asm_mut().nptr(Type::Int(Int::I8)),
                    void_ptr,
                    void_ptr,
                    ctx.asm_mut().nptr(int8_ptr),
                    ctx.asm_mut().nptr(int8_ptr),
                ],
                Type::Int(Int::I32),
            ),
            true,
        ),
        "pidfd_getpid" => CallSite::builtin(
            "pidfd_getpid".into(),
            FnSig::new(&[Type::Int(Int::I32)], Type::Int(Int::I32)),
            true,
        ),
        _ => {
            todo!("Unsuported function refered to using a weak static. Function name is {name:?}.")
        }
    }
}
