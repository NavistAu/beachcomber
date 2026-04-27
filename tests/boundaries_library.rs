use beachcomber::boundaries::library::{LibloadingLoader, LibraryLoader};

#[test]
#[ignore = "real impl wired in P3.8"]
fn libloading_loader_is_a_trait_object() {
    let _: Box<dyn LibraryLoader> = Box::new(LibloadingLoader);
}
