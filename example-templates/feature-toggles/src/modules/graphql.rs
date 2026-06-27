//! GraphQL module (included because `graphql` was selected).

pub fn schema() -> &'static str {
    "type Query { _empty: String }"
}
