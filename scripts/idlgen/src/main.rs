//! `idlgen <program dir> <idl.json> <types.ts>`: the IDL (spec 0.1.0) and its
//! TS type the way `anchor build` 1.2.2 writes them (`target/idl/<name>.json`,
//! `target/types/<name>.ts`), without anchor-cli.
//!
//! The IDL comes from anchor-lang-idl 0.1.4's `IdlBuilder`, the builder
//! anchor-cli 1.2.2 calls: it runs `cargo test __anchor_private_print_idl
//! --features idl-build` in the program directory (here with `--lib`) and
//! assembles what those tests print. `resolution` is off, as in Anchor.toml.
//! scripts/gen-idl.sh runs this on dev-dependency-free copies of the programs.
//!
//! Toolchain: the builder runs `cargo`, and with RUSTUP_TOOLCHAIN set it
//! passes a literal "+{toolchain}" argument (anchor-lang-idl 0.1.4 forgot the
//! `format!`), which cargo rejects. rustup sets RUSTUP_TOOLCHAIN for
//! `cargo +<toolchain> run`, so it is turned into PATH here: that toolchain's
//! own `bin` first, the variable removed.
use anchor_lang_idl::{build::IdlBuilder, types::Idl};
use anyhow::{anyhow, bail, Context, Result};
use heck::{ToLowerCamelCase, ToPascalCase};
use serde_json::Value;
use std::{env, ffi::OsString, path::PathBuf, process::Command};

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        bail!("usage: idlgen <program dir> <idl.json> <types.ts>");
    }
    let (program_dir, idl_out, ts_out) = (&args[1], &args[2], &args[3]);

    pin_toolchain_on_path()?;
    let idl = IdlBuilder::new()
        .program_path(PathBuf::from(program_dir).canonicalize()?)
        .resolution(false)
        .cargo_args(vec!["--lib".into()])
        .build()
        .with_context(|| format!("building the IDL of {program_dir}"))?;

    // anchor-cli's write_idl
    std::fs::write(idl_out, serde_json::to_string_pretty(&idl)?)?;
    std::fs::write(ts_out, idl_ts(&idl)?)?;
    Ok(())
}

/// RUSTUP_TOOLCHAIN=<t> becomes PATH=<sysroot of t>/bin:$PATH (module docs).
fn pin_toolchain_on_path() -> Result<()> {
    if env::var_os("RUSTUP_TOOLCHAIN").is_none() {
        return Ok(());
    }
    let out = Command::new("rustc").args(["--print", "sysroot"]).output()?;
    if !out.status.success() {
        bail!("rustc --print sysroot failed");
    }
    let sysroot = PathBuf::from(String::from_utf8(out.stdout)?.trim());
    let mut paths = vec![sysroot.join("bin")];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_else(OsString::new)));
    env::set_var("PATH", env::join_paths(paths)?);
    env::remove_var("RUSTUP_TOOLCHAIN");
    Ok(())
}

/// anchor-cli 1.2.2's `idl_ts`: the IDL with its identifiers in camelCase, as
/// a type (`Program<T>`); the JSON file stays the program's IDL.
fn idl_ts(idl: &Idl) -> Result<String> {
    let idl_name = &idl.metadata.name;
    let type_name = idl_name.to_pascal_case();
    let mut camel_idl = serde_json::to_value(idl)?;
    camel_case_idl_identifiers(&mut camel_idl);
    let camel_idl = serde_json::to_string_pretty(
        &serde_json::from_value::<Idl>(camel_idl).map_err(|e| anyhow!("camelCase IDL: {e}"))?,
    )?;

    Ok(format!(
        r#"/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/{idl_name}.json`.
 */
export type {type_name} = {camel_idl};
"#
    ))
}

fn camel_case_idl_identifiers(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(camel_case_idl_identifiers),
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "name" | "path" | "account" | "relations" | "generic") {
                    camel_case_idl_identifier(value);
                } else {
                    camel_case_idl_identifiers(value);
                }
            }
        }
        _ => {}
    }
}

fn camel_case_idl_identifier(value: &mut Value) {
    match value {
        // anchor-cli leaves anything that parses as a Pubkey alone
        Value::String(s) if !is_pubkey(s) => {
            *s = s.split('.').map(|p| p.to_lower_camel_case()).collect::<Vec<_>>().join(".");
        }
        Value::String(_) => {}
        Value::Array(values) => values.iter_mut().for_each(camel_case_idl_identifier),
        _ => camel_case_idl_identifiers(value),
    }
}

/// `Pubkey::try_from(&str).is_ok()`: base58 of exactly 32 bytes
fn is_pubkey(s: &str) -> bool {
    s.len() <= 44 && bs58::decode(s).into_vec().is_ok_and(|b| b.len() == 32)
}
