use super::expand_large_stack_test;
use syn::ItemFn;
use syn::parse_quote;

fn has_attr(item: &ItemFn, name: &str) -> bool {
    item.attrs.iter().any(|attr| attr.path().is_ident(name))
}

#[test]
fn adds_test_attribute_when_missing() {
    let item: ItemFn = parse_quote! {
        fn sample() {}
    };

    let expanded_tokens = expand_large_stack_test(item);
    let expanded: ItemFn = match syn::parse2(expanded_tokens) {
        Ok(expanded) => expanded,
        Err(error) => panic!("failed to parse expanded function: {error}"),
    };

    assert!(has_attr(&expanded, "test"));
    let body = quote::quote!(#expanded).to_string();
    assert!(body.contains("stack_size"));
}

#[test]
fn removes_tokio_test_and_keeps_test_case() {
    let item: ItemFn = parse_quote! {
        #[test_case(1)]
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn sample(value: usize) -> anyhow::Result<()> {
            let _ = value;
            Ok(())
        }
    };

    let expanded_tokens = expand_large_stack_test(item);
    let expanded: ItemFn = match syn::parse2(expanded_tokens) {
        Ok(expanded) => expanded,
        Err(error) => panic!("failed to parse expanded function: {error}"),
    };

    assert!(has_attr(&expanded, "test_case"));
    assert!(!has_attr(&expanded, "test"));
    let body = quote::quote!(#expanded).to_string();
    assert!(body.contains("tokio :: runtime :: Builder"));
    assert!(!body.contains("tokio :: test"));
}
