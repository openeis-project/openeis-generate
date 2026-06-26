//! Custom Liquid filters: case-conversion helpers usable in templates as
//! `{{ name | kebab_case }}`, `{{ name | snake_case }}`, etc.
//!
//! Ported from cargo-generate's `template_filters.rs` (the eight `heck`-based
//! filters). The `rhai` Liquid filter is intentionally not ported — it needs the
//! shared rhai engine wired through the parser, which is a larger feature.
//!
//! The derive macros (`ParseFilter`, `FilterReflection`, `Display_filter`) come
//! from `liquid_derive`; `liquid_core` re-exports them alongside the runtime
//! traits (`Filter`, `Runtime`, `Value`, `ValueView`).

use heck::{
    ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToShoutySnakeCase, ToSnakeCase,
    ToTitleCase, ToUpperCamelCase,
};
use liquid::model;
use liquid_core::{Filter, Runtime, Value, ValueView};
use liquid_derive::{Display_filter, FilterReflection, ParseFilter};
use pastey::paste;

macro_rules! create_case_filter {
    ($name:literal, $ident:ident, $expr:expr) => {
        paste! {
            #[derive(Clone, ParseFilter, FilterReflection)]
            #[filter(
                name = $name,
                description = "Change text to " $name,
                parsed([<$ident Filter>])
            )]
            pub struct [<$ident FilterParser>];

            #[derive(Debug, Default, Display_filter)]
            #[name = $name]
            struct [<$ident Filter>];

            impl Filter for [<$ident Filter>] {
                fn evaluate(
                    &self,
                    input: &dyn ValueView,
                    _runtime: &dyn Runtime,
                ) -> std::result::Result<Value, liquid_core::Error> {
                    let input = input
                        .as_scalar()
                        .ok_or_else(|| liquid_core::Error::with_msg("String expected"))?;

                    #[allow(clippy::redundant_closure_call)]
                    let input = $expr(input.into_string().to_string());
                    Ok(Value::scalar(model::Scalar::from(input)))
                }
            }
        }
    };
}

create_case_filter!("kebab_case", KebabCase, |i: String| i.to_kebab_case());
create_case_filter!("lower_camel_case", LowerCamelCase, |i: String| i
    .to_lower_camel_case());
create_case_filter!("pascal_case", PascalCase, |i: String| i.to_pascal_case());
create_case_filter!("shouty_kebab_case", ShoutyKebabCase, |i: String| i
    .to_shouty_kebab_case());
create_case_filter!("shouty_snake_case", ShoutySnakeCase, |i: String| i
    .to_shouty_snake_case());
create_case_filter!("snake_case", SnakeCase, |i: String| i.to_snake_case());
create_case_filter!("title_case", TitleCase, |i: String| i.to_title_case());
create_case_filter!("upper_camel_case", UpperCamelCase, |i: String| i
    .to_upper_camel_case());

#[cfg(test)]
mod tests {
    use crate::variables::Variables;

    /// Build a parser with the case filters registered and render a template.
    fn render(template: &str, vars: &Variables) -> String {
        let parser = crate::generate::parser_with_filters();
        let object = crate::generate::to_object(vars);
        parser.parse(template).unwrap().render(&object).unwrap()
    }

    #[test]
    fn all_case_filters_render() {
        let mut v = Variables::default();
        v.0.insert("name".into(), "My Cool Project".into());
        let r = |f: &str| render(&format!("{{{{ name | {f} }}}}"), &v);
        assert_eq!(r("kebab_case"), "my-cool-project");
        assert_eq!(r("snake_case"), "my_cool_project");
        assert_eq!(r("pascal_case"), "MyCoolProject");
        assert_eq!(r("lower_camel_case"), "myCoolProject");
        assert_eq!(r("upper_camel_case"), "MyCoolProject");
        assert_eq!(r("shouty_kebab_case"), "MY-COOL-PROJECT");
        assert_eq!(r("shouty_snake_case"), "MY_COOL_PROJECT");
        assert_eq!(r("title_case"), "My Cool Project");
    }
}
