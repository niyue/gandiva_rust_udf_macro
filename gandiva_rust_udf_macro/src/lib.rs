mod udf_macro_test;

extern crate proc_macro;

use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, PatType, ReturnType};

#[proc_macro_attribute]
pub fn udf(
    attrs: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let mut needs_context = false;
    let _ = _extract_needs_context(attrs, &mut needs_context);
    let input = proc_macro2::TokenStream::from(input);
    udf_impl(input, needs_context).into()
}

#[proc_macro]
pub fn context_fns(_input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    context_fns_impl().into()
}

fn _extract_needs_context(
    attrs: proc_macro::TokenStream,
    needs_context: &mut bool,
) -> proc_macro::TokenStream {
    let udf_attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("needs_context") {
            *needs_context = true;
            Ok(())
        } else {
            Err(meta.error("unsupported udf macro attribute"))
        }
    });

    parse_macro_input!(attrs with udf_attr_parser);
    proc_macro::TokenStream::new()
}

fn _extract_params(input: proc_macro2::TokenStream) -> ItemFn {
    syn::parse2(input).unwrap()
}

fn context_fns_impl() -> proc_macro2::TokenStream {
    quote! {
        #[cfg(test)]
        unsafe fn gdv_fn_context_arena_malloc(_context: i64, size: i32) -> *mut i8 {
            let mut buffer = Vec::with_capacity(size as usize);
            let ptr = buffer.as_mut_ptr();
            std::mem::forget(buffer);
            ptr as *mut i8
        }

        #[cfg(test)]
        unsafe fn gdv_fn_context_set_error_msg(_context: i64, error_msg: *const i8) {
            let error_msg_str = std::ffi::CStr::from_ptr(error_msg).to_str().unwrap();
            // eprintln!("Error message: {}", error_msg_str);
        }

        #[cfg(test)]
        pub(crate) fn gdv_fn_context_arena_free(ptr: *mut i8, size: i32) {
            unsafe {
                let buffer = Vec::from_raw_parts(ptr, size as usize, size as usize);
                drop(buffer);
            }
        }
    }
}

fn _map_type(arg_type: &str) -> String {
    match arg_type {
        "i8" => "int8",
        "i16" => "int16",
        "i32" => "int32",
        "i64" => "int64",
        "u8" => "uint8",
        "u16" => "uint16",
        "u32" => "uint32",
        "u64" => "uint64",
        "f32" => "float32",
        "f64" => "float64",
        "& str" => "utf8",
        _ => arg_type,
    }
        .to_string()
}

fn udf_impl(input: proc_macro2::TokenStream, needs_context: bool) -> proc_macro2::TokenStream {
    let function = _extract_params(input);
    let function_name = &function.sig.ident;
    let return_type = &function.sig.output;

    let mut wrapper_args = Vec::new();
    let mut call_args = Vec::new();
    let mut arg_types = Vec::new();

    if needs_context {
        wrapper_args.push(quote! { ctx: i64 });
    }

    for input in &function.sig.inputs {
        if let FnArg::Typed(PatType { ty, pat, .. }) = input {
            let arg_name = pat;
            let arg_type = quote!(#ty).to_string();
            // if arg_type is ["i8" | "i16" | "i32" | "i64"] ==> ["int_8" | "int_16" | "int_32" | "int_64"]
            let arg_type_str = arg_type.as_str();
            let mapped_gdv_arg_type = _map_type(arg_type_str);

            if mapped_gdv_arg_type == "utf8" {
                let arg_name_len = format_ident!("{}_len", quote!(#arg_name).to_string());
                wrapper_args.push(quote! { #arg_name: *const libc::c_char, #arg_name_len: i32 });
                call_args.push(quote! { std::str::from_utf8(
                    unsafe { std::slice::from_raw_parts(#arg_name as *const u8, #arg_name_len as usize) }
                ).unwrap() });
            } else {
                wrapper_args.push(quote! { #arg_name: #ty });
                call_args.push(quote! { #arg_name });
            }
            arg_types.push(mapped_gdv_arg_type);
        }
    }

    let wrapper_name = format_ident!("{}_{}", function_name, arg_types.join("_"));

    let expanded = match return_type {
        ReturnType::Default => {
            panic!("The function to be wrapped must have a return type.");
        }
        ReturnType::Type(_, ty) => {
            // if return type is String, use gandiva context function to allocate memory
            let return_type_str = quote!(#ty).to_string();
            let mut return_arrow_type = _map_type(return_type_str.as_str());
            let wrapper_func = if return_type_str == "String" {
                return_arrow_type = "utf8".to_string();
                wrapper_args.push(quote! { out_len: *mut i32 });
                quote! {
                    // output the original function
                    #function

                    #[no_mangle]
                    pub extern "C" fn #wrapper_name(#(#wrapper_args),*) -> *mut libc::c_char {
                        let result = #function_name(#(#call_args),*);
                        return_gdv_string(ctx, &result, out_len)
                    }
                }
            } else {
                quote! {
                    // output the original function
                    #function

                    #[no_mangle]
                    pub extern "C" fn #wrapper_name(#(#wrapper_args),*) -> #ty {
                        #function_name(#(#call_args),*)
                    }
                }
            };

            let arg_types_quotes = arg_types.iter().map(|arg_type| {
                quote! {#arg_type.to_string()}
            });
            let base_name_str = function_name.to_string();
            let pc_name_str = wrapper_name.to_string();
            // register the wrapper function metadata
            let register_func_ident = format_ident!("register_{}", wrapper_name);
            let register_func_meta = quote! {
                pub fn #register_func_ident() {
                    gandiva_rust_udf_shared::register_udf(gandiva_rust_udf_shared::UdfMetaData {
                        base_name: #base_name_str.to_string(),
                        param_types: vec![#(#arg_types_quotes),*],
                        return_type: #return_arrow_type.to_string(),
                        pc_name: #pc_name_str.to_string(),
                    });
                }
            };

            quote! {
                #wrapper_func
                #register_func_meta
            }
        }
    };

    expanded
}