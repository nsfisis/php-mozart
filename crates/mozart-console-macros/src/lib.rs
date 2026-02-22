mod codegen;
mod parser;

use proc_macro::TokenStream;
use syn::Expr;
use syn::punctuated::Punctuated;

/// Format a string with Symfony Console-style tags.
///
/// Supported tags: `<info>`, `<comment>`, `<error>`, `<question>`, `<highlight>`, `<warning>`.
///
/// # Examples
///
/// ```ignore
/// // Single tagged segment
/// console_format!("<info>All packages are up to date.</info>")
///
/// // With format arguments
/// console_format!("<info>Removing {name} from require-dev</info>")
///
/// // Mixed tags
/// console_format!("<info>{}</info> : <comment>{}</comment>", label, value)
///
/// // Plain text (equivalent to format!)
/// console_format!("plain text {}", x)
/// ```
#[proc_macro]
pub fn console_format(input: TokenStream) -> TokenStream {
    let input2: proc_macro2::TokenStream = input.into();
    match console_format_impl(input2) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

fn console_format_impl(
    input: proc_macro2::TokenStream,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    let args: ConsoleFormatArgs = syn::parse2(input)?;
    let segments = parser::parse_format_string(&args.format_str)
        .map_err(|msg| syn::Error::new(args.format_str_span, msg))?;
    Ok(codegen::generate(&segments, &args.extra_args))
}

struct ConsoleFormatArgs {
    format_str: String,
    format_str_span: proc_macro2::Span,
    extra_args: Punctuated<Expr, syn::Token![,]>,
}

impl syn::parse::Parse for ConsoleFormatArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let lit: syn::LitStr = input.parse()?;
        let format_str = lit.value();
        let format_str_span = lit.span();

        let extra_args = if input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
            Punctuated::parse_terminated(input)?
        } else {
            Punctuated::new()
        };

        Ok(ConsoleFormatArgs {
            format_str,
            format_str_span,
            extra_args,
        })
    }
}
