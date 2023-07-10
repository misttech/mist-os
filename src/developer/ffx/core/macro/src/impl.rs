// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lazy_static::lazy_static;
use proc_macro2::{Punct, Span, TokenStream};
use quote::{quote, ToTokens};
use std::collections::HashMap;
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    spanned::Spanned,
    token::Comma,
    AngleBracketedGenericArguments, Error, FnArg, GenericArgument, Ident, ItemFn, ItemStruct, Lit,
    LitStr, Pat, PatIdent, PatType, PathArguments, PathSegment, Token,
    Type::Path,
    TypePath,
};

const ATTRIBUTE_ON_WRONG_PROXY_TYPE: &str = "The ffx attribute for specifying the output type is \
                                             only recognized on the Writer type.";

const UNKNOWN_PROXY_TYPE: &str = "This argument was not recognized. Possible arguments include \
                                  proxy types as well as Result and Option wrappers for proxy \
                                  types.";

const EXPECTED_SIGNATURE: &str = "ffx_plugin expects at least the command created in the args.rs \
                                  file and will accept FIDL proxies if mapped in the ffx_plugin \
                                  annotation.";

const UNRECOGNIZED_PARAMETER: &str = "If this is a proxy, make sure the parameter's type matches \
                                      the mapping passed into the ffx_plugin attribute.";

const DAEMON_PROTOCOL_IDENT: &str = "daemon::protocol";

const CONNECT_TO_CUSTOM_PROTOCOL_ERROR: &str =
    "Connecting to custom target protocols is no longer \
     supported by ffx_plugin. Please migrate to use fho. See: \
     https://fuchsia.dev/fuchsia-src/development/tools/ffx/development/subtools/migrating?hl=en";

lazy_static! {
    static ref KNOWN_PROXIES: Vec<(&'static str, &'static str, bool)> = vec![
        ("RemoteControlProxy", "remote_factory", true),
        ("DaemonProxy", "daemon_factory", true),
        ("FastbootProxy", "fastboot_factory", true),
        ("TargetProxy", "target_factory", true),
        ("VersionInfo", "build_info", false),
        ("Writer", "writer", false),
    ];
}

pub fn ffx_command(input: ItemStruct) -> TokenStream {
    let cmd = input.ident.clone();
    quote! {
        #input
        pub type FfxPluginCommand = #cmd;
    }
}

fn qualified_name(path: &syn::Path) -> String {
    path.segments
        .pairs()
        .map(|pair| {
            if pair.punct().is_some() {
                format!("{}::", pair.value().ident.to_string())
            } else {
                // last ident won't have a punctuation
                pair.value().ident.to_string()
            }
        })
        .fold(String::new(), |accum, elem| format!("{}{}", accum, elem))
}

fn generate_fake_test_proxy_method(
    proxy_name: Ident,
    qualified_proxy_type: &syn::Path,
) -> TokenStream {
    let method_name = Ident::new(&format!("setup_fake_{}", proxy_name), Span::call_site());
    // Oneshot method is needed only for the 'component run' unit tests that leaks memory
    // everywhere unless shut down from the server side.
    let oneshot_method_name =
        Ident::new(&format!("setup_oneshot_fake_{}", proxy_name), Span::call_site());
    quote! {
        #[cfg(test)]
        fn #method_name(
            mut handle_request: impl FnMut(ffx_core::macro_deps::fidl::endpoints::Request<<#qualified_proxy_type as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol>) + 'static
        ) -> #qualified_proxy_type {
            use ffx_core::macro_deps::futures::TryStreamExt;
            let (proxy, mut stream) =
                ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<<#qualified_proxy_type as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol>().unwrap();
            ffx_core::macro_deps::fuchsia_async::Task::local(async move {
                while let Ok(Some(req)) = stream.try_next().await {
                    handle_request(req);
                }
            })
            .detach();
            proxy
        }

        #[cfg(test)]
        fn #oneshot_method_name(
            mut handle_request: impl FnMut(ffx_core::macro_deps::fidl::endpoints::Request<<#qualified_proxy_type as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol>) + 'static
        ) -> #qualified_proxy_type {
            use ffx_core::macro_deps::futures::TryStreamExt;
            let (proxy, mut stream) =
                ffx_core::macro_deps::fidl::endpoints::create_proxy_and_stream::<<#qualified_proxy_type as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol>().unwrap();
            ffx_core::macro_deps::fuchsia_async::Task::local(async move {
                if let Ok(Some(req)) = stream.try_next().await {
                    handle_request(req);
                }
            })
            .detach();
            proxy
        }
    }
}

struct GeneratedCodeParts {
    args: Punctuated<TokenStream, Token!(,)>,
    futures: Punctuated<Ident, Token!(,)>,
    future_results: Punctuated<Ident, Token!(,)>,
    proxies_to_generate: Vec<TokenStream>,
    test_fake_methods_to_generate: Vec<TokenStream>,
    writer_attributes: Vec<syn::Attribute>,
    cmd: FnArg,
}

fn parse_arguments(
    args: Punctuated<FnArg, Comma>,
    proxies: &ProxyMap,
) -> Result<GeneratedCodeParts, Error> {
    let mut inner_args: Punctuated<TokenStream, Token!(,)> = Punctuated::new();
    let mut futures: Punctuated<Ident, Token!(,)> = Punctuated::new();
    let mut future_results: Punctuated<Ident, Token!(,)> = Punctuated::new();
    let mut proxies_to_generate = Vec::new();
    let mut test_fake_methods_to_generate = Vec::<TokenStream>::new();
    let mut writer_attributes = Vec::new();
    let mut cmd: Option<FnArg> = None;
    for arg in &args {
        match arg.clone() {
            FnArg::Receiver(_) => {
                return Err(Error::new(
                    arg.span(),
                    "ffx plugin method signature cannot contain self",
                ))
            }
            FnArg::Typed(PatType { ty, pat, attrs, .. }) => match ty.as_ref() {
                Path(TypePath { path, .. }) => {
                    if let Some(GeneratedProxyParts {
                        arg,
                        fut,
                        fut_res,
                        implementation,
                        testing,
                        ffx_attr,
                    }) = generate_known_proxy(&pat, path, &attrs)?
                    {
                        inner_args.push(arg);
                        futures.push(fut);
                        future_results.push(fut_res);
                        proxies_to_generate.push(implementation);
                        test_fake_methods_to_generate.push(testing);
                        match ffx_attr {
                            Some(t) => writer_attributes.push(t),
                            None => {}
                        }
                    } else if let Some(GeneratedProxyParts {
                        arg,
                        fut,
                        fut_res,
                        implementation,
                        testing,
                        ffx_attr,
                    }) = generate_mapped_proxy(proxies, path)?
                    {
                        if extract_ffx_attribute_tokens(&attrs).is_some() {
                            return Err(Error::new(arg.span(), ATTRIBUTE_ON_WRONG_PROXY_TYPE));
                        }
                        inner_args.push(arg);
                        futures.push(fut);
                        future_results.push(fut_res);
                        proxies_to_generate.push(implementation);
                        test_fake_methods_to_generate.push(testing);
                        assert!(ffx_attr.is_none());
                    } else if let Some(GeneratedProxyParts {
                        arg,
                        fut,
                        fut_res,
                        implementation,
                        testing,
                        ffx_attr,
                    }) = generate_daemon_protocol_proxy(proxies, &pat, path)?
                    {
                        if extract_ffx_attribute_tokens(&attrs).is_some() {
                            return Err(Error::new(arg.span(), ATTRIBUTE_ON_WRONG_PROXY_TYPE));
                        }
                        inner_args.push(arg);
                        futures.push(fut);
                        future_results.push(fut_res);
                        proxies_to_generate.push(implementation);
                        test_fake_methods_to_generate.push(testing);
                        assert!(ffx_attr.is_none());
                    } else if let Some(command) = parse_argh_command(&pat) {
                        // This SHOULD be the argh command - and there should only be one.
                        if cmd.is_some() {
                            return Err(Error::new(
                                arg.span(),
                                format!(
                                    "ffx_plugin could not recognize the parameters: {} \n{}",
                                    command.ident.clone(),
                                    UNRECOGNIZED_PARAMETER
                                ),
                            ));
                        }

                        if extract_ffx_attribute_tokens(&attrs).is_some() {
                            return Err(Error::new(arg.span(), ATTRIBUTE_ON_WRONG_PROXY_TYPE));
                        }
                        let ident = command.ident.clone();
                        match (command.mutability, arg.clone()) {
                            (Some(_), FnArg::Typed(p)) => {
                                let new_pat =
                                    Box::new(Pat::Ident(PatIdent { mutability: None, ..command }));
                                cmd = Some(FnArg::Typed(PatType { ty, pat: new_pat, ..p }));
                            }
                            (_, a) => cmd = Some(a),
                        }
                        inner_args.push(quote! { #ident });
                    } else {
                        if let Pat::Ident(pat_ident) = pat.as_ref() {
                            return Err(Error::new(
                                arg.span(),
                                format!(
                                    "ffx_plugin could not recognize the parameter: {}\n{}",
                                    pat_ident.ident.clone(),
                                    UNRECOGNIZED_PARAMETER
                                ),
                            ));
                        } else {
                            return Err(Error::new(arg.span(), EXPECTED_SIGNATURE));
                        }
                    }
                }
                _ => return Err(Error::new(arg.span(), EXPECTED_SIGNATURE)),
            },
        }
    }

    if let Some(cmd) = cmd {
        Ok(GeneratedCodeParts {
            args: inner_args,
            futures,
            future_results,
            proxies_to_generate,
            test_fake_methods_to_generate,
            writer_attributes,
            cmd,
        })
    } else {
        Err(Error::new(args.span(), EXPECTED_SIGNATURE))
    }
}

fn parse_argh_command(pattern_type: &Box<Pat>) -> Option<PatIdent> {
    if let Pat::Ident(pat_ident) = pattern_type.as_ref() {
        Some(pat_ident.clone())
    } else {
        None
    }
}

enum ProxyWrapper<'a> {
    Option(&'a syn::Path),
    Result(&'a syn::Path),
    None(&'a syn::Path),
}

impl ProxyWrapper<'_> {
    pub fn unwrap(&self) -> &syn::Path {
        match *self {
            ProxyWrapper::Option(ref p) => p,
            ProxyWrapper::Result(ref p) => p,
            ProxyWrapper::None(ref p) => p,
        }
    }

    pub fn result_stream(&self, result: &Ident) -> TokenStream {
        match self {
            ProxyWrapper::Option(_) => quote! { #result.ok() },
            ProxyWrapper::Result(_) => quote! { #result },
            ProxyWrapper::None(_) => quote! { #result? },
        }
    }

    pub fn map_result_stream(&self, result: &Ident, mapping: &Ident) -> TokenStream {
        match self {
            ProxyWrapper::Option(_) => quote! { #result.map(|_| #mapping).ok() },
            ProxyWrapper::Result(_) => quote! { #result.map(|_| #mapping) },
            ProxyWrapper::None(_) => quote! { #result.map(|_| #mapping)? },
        }
    }
}

fn extract_proxy_type(proxy_type_path: &syn::Path) -> Result<ProxyWrapper<'_>, Error> {
    if proxy_type_path.segments.last().is_none() {
        return Err(Error::new(proxy_type_path.span(), UNKNOWN_PROXY_TYPE));
    }
    let simple_proxy_type =
        proxy_type_path.segments.last().expect("proxy path should not be empty");
    match &simple_proxy_type.arguments {
        PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) => {
            match args.first() {
                Some(GenericArgument::Type(Path(TypePath { path, .. }))) => {
                    let option_ident = Ident::new("Option", Span::call_site());
                    let result_ident = Ident::new("Result", Span::call_site());
                    match path.segments.last() {
                        Some(PathSegment { .. }) => {
                            if simple_proxy_type.ident == option_ident {
                                Ok(ProxyWrapper::Option(&path))
                            } else if simple_proxy_type.ident == result_ident {
                                Ok(ProxyWrapper::Result(&path))
                            } else {
                                Err(Error::new(simple_proxy_type.span(), UNKNOWN_PROXY_TYPE))
                            }
                        }
                        _ => Err(Error::new(simple_proxy_type.span(), UNKNOWN_PROXY_TYPE)),
                    }
                }
                _ => Err(Error::new(simple_proxy_type.span(), UNKNOWN_PROXY_TYPE)),
            }
        }
        PathArguments::None => Ok(ProxyWrapper::None(&proxy_type_path)),
        _ => Err(Error::new(simple_proxy_type.span(), UNKNOWN_PROXY_TYPE)),
    }
}

fn extract_ffx_attribute_tokens(attrs: &Vec<syn::Attribute>) -> Option<&syn::Attribute> {
    if !attrs.is_empty() {
        for attr in attrs.iter() {
            if attr.path.is_ident("ffx") {
                return Some(attr);
            }
        }
    }
    None
}

fn generate_known_proxy(
    pattern_type: &Box<Pat>,
    path: &syn::Path,
    attrs: &Vec<syn::Attribute>,
) -> Result<Option<GeneratedProxyParts>, Error> {
    let proxy_wrapper_type = extract_proxy_type(path)?;
    let proxy_type_path = proxy_wrapper_type.unwrap();
    let proxy_type = match proxy_type_path.segments.last() {
        Some(last) => last,
        None => return Err(Error::new(proxy_type_path.span(), UNKNOWN_PROXY_TYPE)),
    };
    let ffx_attr = extract_ffx_attribute_tokens(attrs);
    for known_proxy in KNOWN_PROXIES.iter() {
        if proxy_type.ident == Ident::new(known_proxy.0, Span::call_site()) {
            if ffx_attr.is_some() && known_proxy.0 != "Writer" {
                return Err(Error::new(proxy_type.span(), ATTRIBUTE_ON_WRONG_PROXY_TYPE));
            }
            if let Pat::Ident(pat_ident) = pattern_type.as_ref() {
                let factory_name = Ident::new(known_proxy.1, Span::call_site());
                let output_fut = Ident::new(&format!("{}_fut", factory_name), Span::call_site());
                let output_fut_res =
                    Ident::new(&format!("{}_fut_res", factory_name), Span::call_site());
                let implementation = quote! {
                    let #output_fut = async {
                        let retry_count = 1;
                        let mut tries = 0;
                        loop {
                            tries += 1;
                            let factory = injector.#factory_name().await;
                            if factory.is_ok() || tries > retry_count {
                                break factory;
                            }
                        }
                    };
                };

                let testing = if known_proxy.2 {
                    generate_fake_test_proxy_method(pat_ident.ident.clone(), proxy_type_path)
                } else {
                    quote! {}
                };

                let arg = proxy_wrapper_type.result_stream(&output_fut_res);
                return Ok(Some(GeneratedProxyParts {
                    arg,
                    fut: output_fut,
                    fut_res: output_fut_res,
                    implementation,
                    testing,
                    ffx_attr: ffx_attr.cloned(),
                }));
            }
        }
    }
    Ok(None)
}

fn generate_daemon_protocol_proxy(
    proxies: &ProxyMap,
    pattern_type: &Box<Pat>,
    path: &syn::Path,
) -> Result<Option<GeneratedProxyParts>, Error> {
    let proxy_wrapper_type = extract_proxy_type(path)?;
    let proxy_type_path = proxy_wrapper_type.unwrap();
    let daemon_protocol_name = qualified_name(proxy_type_path);
    let res = proxies.map.get(&daemon_protocol_name).and_then(|mapping| {
        if mapping != DAEMON_PROTOCOL_IDENT {
            return None;
        }
        if let Pat::Ident(pat_ident) = pattern_type.as_ref() {
            let output = pat_ident.ident.clone();
            let output_fut = Ident::new(&format!("{}_fut", output), Span::call_site());
            let output_fut_res = Ident::new(&format!("{}_fut_res", output), Span::call_site());
            let server_end = Ident::new(&format!("{}_server_end", output), Span::call_site());
            // TODO(awdavies): When there is a component to test if a protocol exists, add the test
            // command for it in the daemon.
            let implementation = quote! {
                let (#output, #server_end) = ffx_core::macro_deps::fidl::endpoints::create_endpoints::<<#path as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol>();
                let #output = #output.into_proxy()?;
                let #output_fut;
                {
                    let svc_name = <<#path as ffx_core::macro_deps::fidl::endpoints::Proxy>::Protocol as ffx_core::macro_deps::fidl::endpoints::DiscoverableProtocolMarker>::PROTOCOL_NAME;
                    use ffx_core::macro_deps::futures::TryFutureExt;
                    #output_fut = injector.daemon_factory().await?.connect_to_protocol(
                        svc_name,
                        #server_end.into_channel(),
                    ).map_ok_or_else(|e| ffx_core::macro_deps::anyhow::Result::<()>::Err(ffx_core::macro_deps::anyhow::anyhow!(e)), move |fidl_result| {
                        fidl_result
                        .map(|_| ())
                        .map_err(|e| match e {
                            ffx_core::macro_deps::fidl_fuchsia_developer_ffx::DaemonError::ProtocolNotFound =>
                                ffx_core::macro_deps::errors::ffx_error!(
                                    format!(
"The daemon protocol '{}' did not match any protocols on the daemon
If you are not developing this plugin or the protocol it connects to, then this is a bug

Please report it at http://fxbug.dev/new/ffx+User+Bug.",
                                        svc_name)
                                    ).into(),
                            ffx_core::macro_deps::fidl_fuchsia_developer_ffx::DaemonError::ProtocolOpenError =>
                                ffx_core::macro_deps::errors::ffx_error!(
                                    format!(
"The daemon protocol '{}' failed to open on the daemon.

If you are developing the protocol, there may be an internal failure when invoking the start
function. See the ffx.daemon.log for details at `ffx config get log.dir -p sub`.

If you are NOT developing this plugin or the protocol it connects to, then this is a bug.

Please report it at http://fxbug.dev/new/ffx+User+Bug.",
                                        svc_name
                                    )
                                ).into(),
                            unexpected =>
                                ffx_core::macro_deps::errors::ffx_error!(
                                    format!(
"While attempting to open the daemon protocol '{}', received an unexpected error:

{:?}

This is not intended behavior and is a bug.
Please report it at http://fxbug.dev/new/ffx+User+Bug.",

                                        svc_name,
                                        unexpected,
                                    )
                                ).into(),
                        })
                    });
                }
            };
            let arg = proxy_wrapper_type.map_result_stream(&output_fut_res, &output);
            let testing = generate_fake_test_proxy_method(pat_ident.ident.clone(), path);
            return Some(GeneratedProxyParts {
                arg,
                fut: output_fut,
                fut_res: output_fut_res,
                implementation,
                testing,
                ffx_attr: None,
            })
        }
        None
    });
    Ok(res)
}

struct GeneratedProxyParts {
    arg: TokenStream,
    fut: Ident,
    fut_res: Ident,
    implementation: TokenStream,
    testing: TokenStream,
    ffx_attr: Option<syn::Attribute>,
}

fn generate_mapped_proxy(
    proxies: &ProxyMap,
    path: &syn::Path,
) -> Result<Option<GeneratedProxyParts>, Error> {
    let proxy_wrapper_type = extract_proxy_type(path)?;
    let proxy_type_path = proxy_wrapper_type.unwrap();
    let qualified_proxy_name = qualified_name(proxy_type_path);
    if let Some(mapping) = proxies.map.get(&qualified_proxy_name) {
        let mapping_lit = LitStr::new(mapping, Span::call_site());
        if mapping_lit.value() == DAEMON_PROTOCOL_IDENT {
            return Ok(None);
        }
    }
    Ok(None)
}

fn remove_all_ffx_attrs(mut input: ItemFn) -> ItemFn {
    for arg in input.sig.inputs.iter_mut() {
        if let FnArg::Typed(PatType { attrs, .. }) = arg {
            if !attrs.is_empty() {
                *attrs = attrs
                    .iter_mut()
                    .filter(|attr| !attr.path.is_ident("ffx"))
                    .map(|x| &*x)
                    .cloned()
                    .collect::<Vec<_>>();
            }
        }
    }

    input
}

mod kw {
    syn::custom_keyword!(machine);
}

#[derive(Debug)]
enum WriterArgument {
    Machine { _machine_token: kw::machine, _eq_token: syn::Token![=], ty: syn::Type },
}

impl syn::parse::Parse for WriterArgument {
    fn parse(input: ParseStream<'_>) -> Result<Self, Error> {
        let lookahead = input.lookahead1();
        if lookahead.peek(kw::machine) {
            Ok(WriterArgument::Machine {
                _machine_token: input.parse::<kw::machine>()?,
                _eq_token: input.parse()?,
                ty: input.parse()?,
            })
        } else {
            Err(lookahead.error())
        }
    }
}

impl ToTokens for WriterArgument {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            WriterArgument::Machine { ty, .. } => ty.to_tokens(tokens),
        }
    }
}

pub fn ffx_plugin(input: ItemFn, proxies: ProxyMap) -> Result<TokenStream, Error> {
    let method = input.sig.ident.clone();
    let asyncness = if input.sig.asyncness.is_some() {
        quote! {.await}
    } else {
        quote! {}
    };
    let return_type = input.sig.output.clone();

    let GeneratedCodeParts {
        args,
        futures,
        future_results,
        proxies_to_generate,
        test_fake_methods_to_generate,
        writer_attributes,
        cmd,
    } = parse_arguments(input.sig.inputs.clone(), &proxies)?;

    let mut outer_args: Punctuated<_, Token!(,)> = Punctuated::new();
    outer_args.push(quote! {injector: &::std::sync::Arc<dyn ffx_core::Injector>});
    outer_args.push(quote! {#cmd});

    let writer_attributes: Result<Vec<WriterArgument>, Error> =
        writer_attributes.into_iter().map(|t| t.parse_args::<WriterArgument>()).collect();
    let mut writer_attributes = writer_attributes?;
    let (writers, is_supported) = if writer_attributes.is_empty() {
        (quote! { () }, quote! { false })
    } else {
        assert!(writer_attributes.len() == 1);
        let writer_attribute = writer_attributes.swap_remove(0);
        (quote! { #writer_attribute }, quote! { true })
    };

    let implementation = if proxies_to_generate.len() > 0 {
        quote! {
            #(#proxies_to_generate)*
            let (#future_results,) = ffx_core::macro_deps::futures::join!(#futures);
            #method(#args)#asyncness
        }
    } else {
        quote! {
            #method(#args)#asyncness
        }
    };

    let gated_impl = if let Some(key) = proxies.experiment_key {
        quote! {
            if injector.is_experiment(#key).await {
                #implementation
            } else {
                // The user did not opt-in for the feature. This will exit with a non-zero status
                // after displaying the error message.
                ffx_core::macro_deps::errors::ffx_bail!(
                    "This is an experimental subcommand.  To enable this subcommand run 'ffx config set {} true'",
                    #key
                )
            }
        }
    } else {
        implementation
    };

    let input = remove_all_ffx_attrs(input);

    let res = quote! {
        #input
        pub async fn ffx_plugin_impl(#outer_args) #return_type {
            #gated_impl
        }

        #(#test_fake_methods_to_generate)*

        pub type FfxWriterType = #writers;

        // This is built for the execution library.
        pub fn ffx_plugin_is_machine_supported() -> bool {
            #is_supported
        }
    };
    Ok(res)
}

#[derive(Debug)]
pub struct ProxyMap {
    experiment_key: Option<String>,
    map: HashMap<String, String>,
}

impl Default for ProxyMap {
    fn default() -> Self {
        Self { experiment_key: None, map: HashMap::new() }
    }
}

impl Parse for ProxyMap {
    fn parse(input: ParseStream<'_>) -> Result<Self, Error> {
        let mut experiment_key = None;
        let mut map = HashMap::new();
        while !input.is_empty() {
            if input.peek(Ident) {
                // Dump the next parse since we got it via the peek
                if let Path(TypePath { path, .. }) = input.parse()? {
                    let _: Punct = input.parse()?;
                    if let Lit::Str(selection) = input.parse()? {
                        if input.peek(Token!(,)) {
                            // Parse the trailing comma
                            let _: Punct = input.parse()?;
                        }
                        let selector = if selection.value() == DAEMON_PROTOCOL_IDENT {
                            selection.value()
                        } else {
                            return Err(Error::new(
                                selection.span(),
                                CONNECT_TO_CUSTOM_PROTOCOL_ERROR,
                            ));
                        };
                        map.insert(qualified_name(&path), selector);
                    }
                }
            } else if input.peek(Lit) {
                if let Lit::Str(found_key) = input.parse()? {
                    // This must be the experiment key.
                    if let Some(key) = experiment_key {
                        // experiment_key was already found
                        return Err(Error::new(
                            found_key.span(),
                            format!(
                                "Experiment key set twice.  First found: {}, Second found: {}",
                                key,
                                found_key.value()
                            ),
                        ));
                    } else {
                        experiment_key = Some(format!("{}", found_key.value()));
                        if input.peek(Token!(,)) {
                            // Parse the trailing comma
                            let _: Punct = input.parse()?;
                        }
                    }
                }
            } else {
                return Err(Error::new(Span::call_site(), "Invalid plugin inputs"));
            }
        }
        Ok(Self { map, experiment_key })
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use std::default::Default;
    use syn::{
        parse::{Parse, ParseStream},
        parse2, parse_quote, Attribute, ItemType, ReturnType,
    };

    struct WrappedCommand {
        original: ItemStruct,
        plugin: ItemType,
    }

    impl Parse for WrappedCommand {
        fn parse(input: ParseStream<'_>) -> Result<Self, Error> {
            Ok(WrappedCommand { original: input.parse()?, plugin: input.parse()? })
        }
    }

    #[test]
    fn test_ffx_command() -> Result<(), Error> {
        let item: ItemStruct = parse_quote! {pub struct EchoCommand {}};
        let plugin: ItemType = parse_quote! {pub type FfxPluginCommand = EchoCommand;};
        let result: WrappedCommand = parse2(ffx_command(item.clone()))?;
        assert_eq!(item, result.original);
        assert_eq!(plugin, result.plugin);
        Ok(())
    }

    #[test]
    fn test_ffx_plugin_with_just_a_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(_cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_non_async_ffx_plugin_with_just_a_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub fn echo(_cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_daemon_proxy_and_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                daemon: DaemonProxy,
                _cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_fastboot_proxy_and_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                fastboot: FastbootProxy,
                _cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_remote_proxy_and_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                remote: RemoteControlProxy,
                _cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_remote_proxy_and_daemon_proxy_and_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                daemon: DaemonProxy,
                remote: RemoteControlProxy,
                _cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_remote_proxy_and_daemon_proxy_and_command_out_of_order(
    ) -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                remote: RemoteControlProxy,
                _cmd: EchoCommand,
                daemon: DaemonProxy) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_no_parameters_should_err() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo() -> Result<(), Error> { Ok(()) }
        };
        if ffx_plugin(original.clone(), proxies).is_ok() {
            assert!(false, "A method with no parameters should throw an error");
        }
        Ok(())
    }

    #[test]
    fn test_ffx_plugin_with_self_receiver_should_err() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(self, cmd: EchoCommand) -> Result<(), Error> { Ok(()) }
        };
        if ffx_plugin(original.clone(), proxies).is_ok() {
            assert!(false, "A method with a receiver should throw an error");
        }
        Ok(())
    }

    #[test]
    fn test_ffx_plugin_with_referenced_param_should_err() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(proxy: &TestProxy, cmd: EchoCommand) -> Result<(), Error> { Ok(()) }
        };
        if ffx_plugin(original.clone(), proxies).is_ok() {
            assert!(false, "A method with references should throw an error");
        }
        Ok(())
    }

    #[test]
    fn test_empty_proxy_map_should_not_err() {
        let _proxy_map: ProxyMap = parse_quote! {};
    }

    #[test]
    #[should_panic]
    fn test_map_expose_with_service_fails() {
        let _proxy_map: ProxyMap = parse_quote! {test = "test:expose:anything"};
    }

    #[test]
    fn test_invalid_selection_should_err() {
        let result: Result<ProxyMap, Error> = parse2(quote! {
            test = "test"
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_map_using_out_fails() {
        let result: Result<ProxyMap, Error> = parse2(quote! {
            test = "test:out"
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_map_not_using_expose_and_service_fails() {
        let result: Result<ProxyMap, Error> = parse2(quote! {
            test = "test:anything:anything"
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_map_using_in_fails() {
        let result: Result<ProxyMap, Error> = parse2(quote! {
            test = "test:in:anything"
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_mapping_should_err() {
        let result: Result<ProxyMap, Error> = parse2(quote! {
            test, "test"
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_input_should_err() -> Result<(), Error> {
        let result: Result<ProxyMap, Error> = parse2(quote! {test});
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_experiment_key_is_none_when_empty() -> Result<(), Error> {
        let result: ProxyMap = parse2(quote! {})?;
        assert_eq!(result.experiment_key, None);
        Ok(())
    }

    #[test]
    fn test_experiment_key_literal() -> Result<(), Error> {
        let result: ProxyMap = parse2(quote! {"test"})?;
        assert_eq!(result.experiment_key, Some("test".to_string()));
        Ok(())
    }

    #[test]
    fn test_experiment_key_literal_with_trailing_comma() -> Result<(), Error> {
        let result: ProxyMap = parse2(quote! {"test",})?;
        assert_eq!(result.experiment_key, Some("test".to_string()));
        Ok(())
    }

    #[test]
    fn test_multiple_experiment_keys_should_err() -> Result<(), Error> {
        let ex_key = "test_experimental_key".to_string();
        let ex_key_2 = "test_experimental_key_2".to_string();
        let result: Result<ProxyMap, Error> = parse2(quote! {
            #ex_key,
            #ex_key_2,
        });
        assert!(result.is_err());
        Ok(())
    }

    struct WrappedTestFunctions {
        fake_test: ItemFn,
        fake_oneshot_test: ItemFn,
    }

    impl Parse for WrappedTestFunctions {
        fn parse(input: ParseStream<'_>) -> Result<Self, Error> {
            let fake_test = input.parse()?;
            let fake_oneshot_test = input.parse()?;
            Ok(WrappedTestFunctions { fake_test, fake_oneshot_test })
        }
    }

    #[test]
    fn test_generated_test_functions_should_have_test_attribute() -> Result<(), Error> {
        let test = "test_proxy";
        let proxy_name = Ident::new(&format!("{}", test), Span::call_site());
        let qualified_proxy_type: syn::Path = parse2(quote! { test::TestProxy })?;
        let generated = generate_fake_test_proxy_method(proxy_name, &qualified_proxy_type);
        let result: WrappedTestFunctions = parse2(generated)?;
        let attribute_path: syn::Path = parse_quote! { cfg };
        assert_eq!(result.fake_test.attrs[0].path, attribute_path);
        assert_eq!(result.fake_oneshot_test.attrs[0].path, attribute_path);
        let expected_test_arg = Ident::new("test", Span::call_site());
        let mut attr_args: Ident = Attribute::parse_args(&result.fake_test.attrs[0])?;
        assert_eq!(attr_args, expected_test_arg);
        attr_args = Attribute::parse_args(&result.fake_oneshot_test.attrs[0])?;
        assert_eq!(attr_args, expected_test_arg);
        Ok(())
    }

    #[test]
    fn test_generated_test_functions_should_have_expected_names() -> Result<(), Error> {
        let test = "test_proxy";
        let proxy_name = Ident::new(&format!("{}", test), Span::call_site());
        let qualified_proxy_type: syn::Path = parse2(quote! { test::TestProxy })?;
        let generated = generate_fake_test_proxy_method(proxy_name, &qualified_proxy_type);
        let result: WrappedTestFunctions = parse2(generated)?;
        let expected_name = Ident::new(&format!("setup_fake_{}", test), Span::call_site());
        let expected_oneshot_name =
            Ident::new(&format!("setup_oneshot_fake_{}", test), Span::call_site());
        assert_eq!(result.fake_test.sig.ident, expected_name);
        assert_eq!(result.fake_oneshot_test.sig.ident, expected_oneshot_name);
        Ok(())
    }

    #[test]
    fn test_generated_test_functions_should_have_expected_return_type() -> Result<(), Error> {
        let test = "test_proxy";
        let proxy_name = Ident::new(&format!("{}", test), Span::call_site());
        let qualified_proxy_type: syn::Path = parse2(quote! { test::TestProxy })?;
        let generated = generate_fake_test_proxy_method(proxy_name, &qualified_proxy_type);
        let result: WrappedTestFunctions = parse2(generated)?;
        match result.fake_test.sig.output {
            ReturnType::Type(_, output_type) => match output_type.as_ref() {
                Path(TypePath { path, .. }) => assert_eq!(path, &qualified_proxy_type),
                _ => return Err(Error::new(Span::call_site(), "unexpected return type")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected return type")),
        }
        match result.fake_oneshot_test.sig.output {
            ReturnType::Type(_, output_type) => match output_type.as_ref() {
                Path(TypePath { path, .. }) => assert_eq!(path, &qualified_proxy_type),
                _ => return Err(Error::new(Span::call_site(), "unexpected return type")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected return type")),
        }
        Ok(())
    }

    #[test]
    fn test_generate_known_proxy_works_with_daemon_proxy() -> Result<(), Error> {
        let input: ItemFn = parse_quote!(
            fn test_fn(test_param: DaemonProxy) {}
        );
        let param = input.sig.inputs[0].clone();
        if let Some(GeneratedProxyParts { arg, fut, .. }) = match param {
            FnArg::Typed(PatType { ty, pat, attrs, .. }) => match ty.as_ref() {
                Path(TypePath { path, .. }) => generate_known_proxy(&pat, path, &attrs)?,
                _ => return Err(Error::new(Span::call_site(), "unexpected param")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected param")),
        } {
            assert_eq!(arg.to_string(), quote! { daemon_factory_fut_res? }.to_string());
            assert_eq!(fut.to_string(), quote! { daemon_factory_fut }.to_string());
            Ok(())
        } else {
            Err(Error::new(Span::call_site(), "known proxy not generated"))
        }
    }

    #[test]
    fn test_generate_known_proxy_works_with_remote_proxy() -> Result<(), Error> {
        let input: ItemFn = parse_quote!(
            fn test_fn(test_param: RemoteControlProxy) {}
        );
        let param = input.sig.inputs[0].clone();
        if let Some(GeneratedProxyParts { arg, fut, .. }) = match param {
            FnArg::Typed(PatType { ty, pat, attrs, .. }) => match ty.as_ref() {
                Path(TypePath { path, .. }) => generate_known_proxy(&pat, path, &attrs)?,
                _ => return Err(Error::new(Span::call_site(), "unexpected param")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected param")),
        } {
            assert_eq!(arg.to_string(), quote! { remote_factory_fut_res? }.to_string());
            assert_eq!(fut.to_string(), quote! { remote_factory_fut }.to_string());
            Ok(())
        } else {
            Err(Error::new(Span::call_site(), "known proxy not generated"))
        }
    }

    #[test]
    fn test_generate_known_proxy_works_with_fastboot_proxy() -> Result<(), Error> {
        let input: ItemFn = parse_quote!(
            fn test_fn(test_param: FastbootProxy) {}
        );
        let param = input.sig.inputs[0].clone();
        if let Some(GeneratedProxyParts { arg, fut, .. }) = match param {
            FnArg::Typed(PatType { ty, pat, attrs, .. }) => match ty.as_ref() {
                Path(TypePath { path, .. }) => generate_known_proxy(&pat, path, &attrs)?,
                _ => return Err(Error::new(Span::call_site(), "unexpected param")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected param")),
        } {
            assert_eq!(arg.to_string(), quote! { fastboot_factory_fut_res? }.to_string());
            assert_eq!(fut.to_string(), quote! { fastboot_factory_fut }.to_string());
            Ok(())
        } else {
            Err(Error::new(Span::call_site(), "known proxy not generated"))
        }
    }

    #[test]
    fn test_generate_known_proxy_does_not_generate_proxy_for_unknown_proxies() -> Result<(), Error>
    {
        let input: ItemFn = parse_quote!(
            fn test_fn(test_param: UnknownProxy) {}
        );
        let param = input.sig.inputs[0].clone();
        let result = match param {
            FnArg::Typed(PatType { ty, pat, attrs, .. }) => match ty.as_ref() {
                Path(TypePath { path, .. }) => generate_known_proxy(&pat, path, &attrs)?,
                _ => return Err(Error::new(Span::call_site(), "unexpected param")),
            },
            _ => return Err(Error::new(Span::call_site(), "unexpected param")),
        };
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_known_proxy_works_with_options() -> Result<(), Error> {
        let proxies = Default::default();
        let input: ItemFn = parse_quote! {
            fn test_fn(test_param: Option<test::DaemonProxy>, cmd: OptionCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(|_| ())
    }

    #[test]
    fn test_known_proxy_works_with_results() -> Result<(), Error> {
        let proxies = Default::default();
        let input: ItemFn = parse_quote! {
            fn test_fn(test_param: Result<DaemonProxy>, cmd: ResultCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(|_| ())
    }

    #[test]
    fn test_ffx_plugin_with_a_target_proxy_and_command() -> Result<(), Error> {
        let proxies = Default::default();
        let original: ItemFn = parse_quote! {
            pub async fn echo(
                target: TargetProxy,
                _cmd: EchoCommand) -> anyhow::Result<()> { Ok(()) }
        };
        ffx_plugin(original.clone(), proxies).map(|_| ())
    }

    #[test]
    fn test_known_proxy_works_with_custom_return_type() -> Result<(), Error> {
        let proxies = Default::default();
        let input: ItemFn = parse_quote! {
            fn test_fn(cmd: OptionCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(|_| ())
    }

    #[test]
    fn test_service_proxy_works() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
            fn test_fn(test_param: TestProxy, cmd: ResultCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(drop)
    }

    #[test]
    fn test_service_proxy_works_with_option() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
                fn test_fn(test_param: Option<TestProxy>, cmd: ResultCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(drop)
    }

    #[test]
    fn test_service_proxy_works_with_result() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
                fn test_fn(test_param: Result<TestProxy>, cmd: ResultCommand) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(drop)
    }

    #[test]
    fn test_ffx_writer_attribute_fails_if_on_command() -> Result<(), Error> {
        let proxies = Default::default();
        let input: ItemFn = parse_quote! {
            pub async fn test_fn(
                #[ffx(machine=Vec<String>)]
                cmd: OptionCommand,
            ) -> Result<()> {}
        };
        assert!(ffx_plugin(input, proxies).map(drop).is_err());
        Ok(())
    }

    #[test]
    fn test_ffx_writer_attribute_fails_if_on_service_proxy() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
            pub async fn test_fn(
                #[ffx(machine=Vec<String>)]
                test_param: TestProxy,
                writer: Writer,
                cmd: OptionCommand,
            ) -> Result<()> {}
        };
        assert!(ffx_plugin(input, proxies).map(drop).is_err());
        Ok(())
    }

    #[test]
    fn test_ffx_writer_attribute_fails_if_on_known_nonwriter_proxy() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
            pub async fn test_fn(
                #[ffx(machine=Vec<String>)]
                remote: RemoteControlProxy,
                writer: Writer,
                cmd: OptionCommand,
            ) -> Result<()> {}
        };
        assert!(ffx_plugin(input, proxies).map(drop).is_err());
        Ok(())
    }

    #[test]
    fn test_ffx_writer_attribute_works_if_on_writer() -> Result<(), Error> {
        let proxies: ProxyMap = parse_quote! { TestProxy = "daemon::protocol" };
        let input: ItemFn = parse_quote! {
            pub async fn test_fn(
                remote: RemoteControlProxy,
                #[ffx(machine = fidl_fuchsia_net::FakeType<String>)]
                writer: Writer,
                cmd: OptionCommand,
            ) -> Result<()> {}
        };
        ffx_plugin(input, proxies).map(drop)
    }
}
