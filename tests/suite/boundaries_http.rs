use beachcomber::boundaries::http::{HttpFetcher, UreqHttpFetcher};

#[test]
#[ignore = "real impl wired in P3.7"]
fn ureq_fetcher_is_a_trait_object() {
    let _: Box<dyn HttpFetcher> = Box::new(UreqHttpFetcher);
}
