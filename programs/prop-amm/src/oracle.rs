// Shared with perps-core (and the keeper) so price handling cannot drift.
// `include!` rather than `#[path]`: Anchor's IDL parser can't follow `#[path]`.
include!("../../perps-core/src/price.rs");
