//! Seed-list token count — **measured, never estimated** (sdk-assembly O1).
//!
//! The 5k~10k-when-it-was-2,454 lesson: an eyeballed budget for a prompt face
//! is wrong often enough to be useless. This test counts the real `api_doc()`
//! string with a real tokenizer and prints the number, so every report can
//! quote a measurement instead of a feeling.

use phi_ns::NamespaceProvider;

/// The seed list the model actually sees — `api_doc()` is the prompt face.
fn seed_list() -> String {
    phi_ns_desktop::PhiNsDesktop::unconnected().api_doc()
}

fn counts(text: &str) -> (usize, usize) {
    let cl100k = tiktoken_rs::cl100k_base_singleton()
        .encode_ordinary(text)
        .len();
    let o200k = tiktoken_rs::o200k_base_singleton()
        .encode_ordinary(text)
        .len();
    (cl100k, o200k)
}

#[test]
fn seed_list_token_count_is_measured_not_estimated() {
    let seed = seed_list();
    let lines = seed.lines().count();
    let (cl100k, o200k) = counts(&seed);
    println!(
        "[seed-tokens] desk.* api_doc(): {} chars, {} lines, cl100k={} tok, o200k={} tok",
        seed.len(),
        lines,
        cl100k,
        o200k
    );

    // One line per method, and the face is the 21-method desktop pack.
    assert_eq!(lines, 21, "seed list must be one line per method:\n{seed}");

    // Regression guard on the size of the prompt face: the seed list is a
    // reference card, not documentation. Measured baseline printed above; a
    // rewrite that doubles the face trips this.
    assert!(
        cl100k < 900,
        "seed list ballooned to {cl100k} cl100k tokens; the model pays this every turn"
    );
    assert!(
        o200k < 900,
        "seed list ballooned to {o200k} o200k tokens; the model pays this every turn"
    );
}

#[test]
fn every_method_carries_its_signature_in_one_line() {
    // The seed list is the model's only reference: each row is
    // `name — signature: explanation`, so a one-line token budget must still
    // carry the call shape.
    let seed = seed_list();
    for line in seed.lines() {
        let (name, doc) = line
            .split_once(" — ")
            .unwrap_or_else(|| panic!("seed line lacks the api_line separator: {line}"));
        assert!(
            name.starts_with("desk."),
            "seed line not rooted at desk: {line}"
        );
        assert!(
            doc.contains('('),
            "seed line carries no call signature: {line}"
        );
    }
}
