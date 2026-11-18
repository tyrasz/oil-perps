//! Generate an Anchor 0.28 IDL (and its TS types file) the way `anchor build` does.
use heck::{ToLowerCamelCase, ToUpperCamelCase};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (lib_rs, idl_out, ts_out) = (&args[1], &args[2], &args[3]);
    let idl = anchor_syn::idl::file::parse(lib_rs, "0.1.0".into(), false, false, true)
        .expect("parse")
        .expect("no idl");
    std::fs::write(idl_out, serde_json::to_string_pretty(&idl).unwrap()).unwrap();

    // anchor-cli's idl_ts: account type names become lowerCamelCase in the TS type
    let mut ts_idl = idl.clone();
    for acc in ts_idl.accounts.iter_mut() {
        acc.name = acc.name.to_lower_camel_case();
    }
    let json = serde_json::to_string_pretty(&ts_idl).unwrap();
    let name = idl.name.to_upper_camel_case();
    std::fs::write(ts_out, format!("export type {name} = {json};\n\nexport const IDL: {name} = {json};\n")).unwrap();
}
