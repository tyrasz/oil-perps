//! The simulated world: perps-core with the OIL market on the CME schedule,
//! mm-registry and orderbook registered as venues, the agents and the keeper,
//! plus instruction builders for the venue programs (the same accounts the
//! programs' integration tests build).

use crate::agents::{Agent, MmBook, Persona, Strategy};
use crate::chain::Chain;
use crate::checks::CheckBook;
use crate::ledger::Ledger;
use crate::metrics::Metrics;
use crate::oracle::{Oracle, OracleConfig};
use crate::predict::{OracleView, Rej};
use crate::scenario::Scenario;
use crate::support::*;
use mm_registry::instructions::{InitializeRegistryParams, PostQuoteParams, UpdateQuoteParams};
use orderbook::PlaceOrderParams;
use perps_core::instructions::{VenueApprovalParams, VenueParams};
use perps_core::state::{LpPool, LpPosition, Market, UserAccount};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use solana_program_test::processor;

pub struct Config {
    pub seed: u64,
    pub days: f64,
    pub tick_secs: i64,
    pub report: Option<String>,
    pub scenario: Scenario,
    pub strict_solvency: bool,
}

fn mm_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    mm_registry::entry(program_id, accounts, data)
}

fn book_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    orderbook::entry(program_id, accounts, data)
}

/// mm-registry's deploy defaults: 50 bps band, 15 s oracle, $50k per fill, the taker signs.
pub fn mm_venue_params() -> VenueParams {
    VenueParams { min_signers: 1, ..default_venue_params() }
}

/// The orderbook's deploy defaults: 100 bps band, 30 s oracle, no signers.
pub fn book_venue_params() -> VenueParams {
    VenueParams { max_price_deviation_bps: 100, max_oracle_age_secs: 30, min_signers: 0, max_fill_notional: 50_000 * USDC }
}

pub const MAX_QUOTE_SIZE: u64 = 1_000 * CONTRACT;

pub struct Sim {
    pub chain: Chain,
    /// Agents' and keeper's randomness
    pub rng: StdRng,
    /// The price path's own stream, so a seed's path does not depend on what the agents do
    pub oracle_rng: StdRng,
    pub cfg: Config,
    pub oracle: Oracle,
    /// Last published oracle price (6 dp) and confidence
    pub px: u64,
    pub conf: u64,
    pub published_at: i64,
    pub now: i64,
    pub tick: u64,
    pub history: Vec<u64>,
    pub registry: Pubkey,
    pub mm_venue: Pubkey,
    pub book: Pubkey,
    pub book_venue: Pubkey,
    pub agents: Vec<Agent>,
    pub keeper: Trader,
    pub ledger: Ledger,
    pub checks: CheckBook,
    pub metrics: Metrics,
    /// Expected vault token balance, from every predicted token flow
    pub vault_expected: i128,
    pub total_minted: u64,
    /// The market as the sim believes it: paused by the guardian, settled by the admin
    pub paused: bool,
    pub settled: bool,
    /// Session state last tick, and whether this tick is the first one closed
    pub was_open: bool,
    /// The session reopened at the last clock move (a tick or an unwind warp)
    pub just_reopened: bool,
    pub at_edge: bool,
    /// Tick of the last reopen after a closed session
    pub last_reopen_tick: u64,
    /// Scripted adversarial actions (adversarial scenario)
    pub adversary: crate::adversary::AdversaryState,
    /// LP wallets: index 0 is the protocol's seed LP (the admin key)
    pub lps: Vec<LpWallet>,
    /// The market and pool as the model left them after the last landed
    /// transaction that touches them (invariants 2a and 2c compare it with
    /// the chain every tick: nothing may move the pool unmodelled)
    pub shadow: Option<crate::model::St>,
    /// Pool series and counters for the report
    pub pool_stats: crate::metrics::PoolStats,
    /// How the last transaction sent failed (None: it landed)
    pub last_failure: Option<crate::chain::TxFailure>,
    /// The last transaction's logs (its events)
    pub last_logs: Vec<String>,
    /// The end-of-run unwind has started
    pub unwinding: bool,
}

/// An LP: the harness's wallet (token account, LpPosition) and what it put in and took out.
pub struct LpWallet {
    pub name: String,
    pub lp: Lp,
    pub minted: u64,
    pub persona: crate::agents::LpPersona,
    /// When it last tried a flow on its own (agents.rs paces itself)
    pub last_try: i64,
    /// Flows it sent on its own: (what, outcome) -> count
    pub tries: std::collections::BTreeMap<String, u64>,
}

impl LpWallet {
    pub fn new(name: &str, lp: Lp, minted: u64, persona: crate::agents::LpPersona) -> LpWallet {
        LpWallet { name: name.into(), lp, minted, persona, last_try: 0, tries: Default::default() }
    }
}

struct Spec {
    name: &'static str,
    persona: Persona,
    usdc: u64,
    strategy: Strategy,
    /// Registers in mm-registry (market makers, and the colluder that quotes)
    registers_mm: bool,
    /// Approves the orderbook venue as taker and maker
    approves_book: bool,
}

impl Spec {
    fn new(name: &'static str, persona: Persona, usdc: u64, strategy: Strategy) -> Spec {
        let registers_mm = persona == Persona::MarketMaker || name == "colluder-a";
        let approves_book = matches!(persona, Persona::Limit | Persona::LimitBig)
            || ["colluder", "gap-", "cross-"].iter().any(|p| name.starts_with(p));
        Spec { name, persona, usdc, strategy, registers_mm, approves_book }
    }
}

impl Sim {
    pub async fn new(cfg: Config) -> Sim {
        let mut pt = ProgramTest::new("mm_registry", mm_registry::ID, processor!(mm_process));
        pt.add_program("orderbook", orderbook::ID, processor!(book_process));
        // Production pool parameters with a one-day withdrawal delay, seeded
        // with the protocol's $1M at t0 (D11, docs/lp-pool.md §16.7)
        let mut base = start_with(pt, crate::chain::pool_params()).await;
        if !is_sbf() {
            crate::quiet_events::install();
        }
        configure(&mut base, cme_config()).await.expect("CME schedule and weekend margins");
        let admin = base.admin.insecure_clone();

        // mm-registry, as in its tests
        let registry = pda(&[b"mm_registry", base.market.as_ref()], &mm_registry::ID);
        let accounts = mm_registry::accounts::InitializeRegistry {
            authority: admin.pubkey(), config: config_pda(), registry, market: base.market,
            collateral_mint: base.mint, system_program: system_program::ID, token_program: spl_token::ID,
        };
        let params = InitializeRegistryParams { min_collateral: 100 * USDC, max_spread: 100, min_quote_size: CONTRACT, max_quote_size: MAX_QUOTE_SIZE, mm_fee: 10 };
        send(&mut base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::InitializeRegistry { params })], &[&admin]).await.unwrap();
        let mm_venue = register_venue(&mut base, &mm_registry::ID, mm_venue_params()).await;

        // orderbook, as in its tests
        let book = pda(&[b"orderbook", base.market.as_ref()], &orderbook::ID);
        let accounts = orderbook::accounts::InitializeOrderBook {
            authority: admin.pubkey(), config: config_pda(), market: base.market, orderbook: book, system_program: system_program::ID,
        };
        send(&mut base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::InitializeOrderbook {})], &[&admin]).await.unwrap();
        let book_venue = register_venue(&mut base, &orderbook::ID, book_venue_params()).await;

        // The keeper: liquidation rewards land in its wallet; it is also the book's matcher
        let keeper = trader(&mut base, 0).await;
        let accounts = orderbook::accounts::SetMatcher { admin: admin.pubkey(), config: config_pda(), orderbook: book };
        send(&mut base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::SetMatcher { matcher: keeper.key() })], &[&admin]).await.unwrap();

        let mut rng = StdRng::seed_from_u64(cfg.seed);
        let now = now(&mut base).await;
        let seed_lp = Lp { kp: base.seed_lp.kp.insecure_clone(), tokens: base.seed_lp.tokens, position: base.seed_lp.position };
        let seed = LpWallet::new("protocol-seed", seed_lp, LP_SEED, crate::agents::LpPersona::Seed);
        let (weekend_gap, daily_gap) = cfg.scenario.gaps();
        let oracle_cfg = OracleConfig { weekend_gap, daily_gap, ..OracleConfig::default() };
        let mut sim = Sim {
            chain: Chain::new(base),
            oracle: Oracle::new(oracle_cfg, OIL_PRICE),
            px: usd(OIL_PRICE),
            conf: 0,
            published_at: now,
            now,
            tick: 0,
            history: Vec::new(),
            registry,
            mm_venue,
            book,
            book_venue,
            agents: Vec::new(),
            keeper,
            ledger: Ledger::default(),
            checks: CheckBook::new(cfg.strict_solvency),
            metrics: Metrics::default(),
            vault_expected: 0,
            // The harness mints the LP pool seed before any agent exists
            total_minted: LP_SEED,
            paused: false,
            settled: false,
            was_open: true,
            just_reopened: false,
            at_edge: false,
            last_reopen_tick: 0,
            adversary: Default::default(),
            lps: vec![seed],
            shadow: None,
            pool_stats: Default::default(),
            last_failure: None,
            last_logs: Vec::new(),
            unwinding: false,
            rng: StdRng::seed_from_u64(rng.gen()),
            oracle_rng: StdRng::seed_from_u64(cfg.seed ^ 0x6f72_6163_6c65),
            cfg,
        };
        for spec in sim.roster(&mut rng) {
            sim.add_agent(spec).await;
        }
        // Public LPs (agents.rs), and the adversarial scenario's sandwiching LP
        let mut lps = vec![
            ("lp-steady", 200_000 * USDC, crate::agents::LpPersona::Steady),
            ("lp-gap", 100_000 * USDC, crate::agents::LpPersona::Gap),
            ("lp-lapse", 50_000 * USDC, crate::agents::LpPersona::Lapse),
        ];
        if sim.cfg.scenario == Scenario::Adversarial {
            lps.push(("lp-sandwich", 300_000 * USDC, crate::agents::LpPersona::Sandwich));
            // Holds a matured request for the withdrawal-gate probes
            lps.push(("lp-probe", 50_000 * USDC, crate::agents::LpPersona::Sandwich));
            // Holds a matured request for the weekend-bound withdrawal probe (D43)
            lps.push(("lp-bound", 200_000 * USDC, crate::agents::LpPersona::Sandwich));
        }
        for (name, usdc, persona) in lps {
            let w = lp(sim.chain.base(), usdc).await;
            sim.total_minted += usdc;
            sim.lps.push(LpWallet::new(name, w, usdc, persona));
        }
        sim.vault_expected = vault_token_balance(sim.chain.base()).await as i128;
        sim.shadow = Some(sim.st().await);
        sim
    }

    /// Two market makers and twelve retail traders, plus the scenario's own agents.
    fn roster(&self, rng: &mut StdRng) -> Vec<Spec> {
        let mut specs = vec![
            Spec::new("mm-tight", Persona::MarketMaker, 60_000 * USDC, Strategy::mm(8.0, 60, 400)),
            Spec::new("mm-wide", Persona::MarketMaker, 40_000 * USDC, Strategy::mm(20.0, 200, 800)),
        ];
        let mut retail = |specs: &mut Vec<Spec>, name: &'static str, persona: Persona| {
            let usdc = rng.gen_range(1_000..=20_000) * USDC;
            let strategy = Strategy::retail(persona, rng);
            specs.push(Spec::new(name, persona, usdc, strategy));
        };
        for name in ["momentum-1", "momentum-2", "momentum-3"] {
            retail(&mut specs, name, Persona::Momentum);
        }
        for name in ["meanrev-1", "meanrev-2"] {
            retail(&mut specs, name, Persona::MeanReversion);
        }
        for name in ["degen-1", "degen-2"] {
            retail(&mut specs, name, Persona::Degen);
        }
        for name in ["limit-1", "limit-2", "limit-3"] {
            retail(&mut specs, name, Persona::Limit);
        }
        for name in ["house-1", "house-2"] {
            retail(&mut specs, name, Persona::House);
        }
        match self.cfg.scenario {
            Scenario::Normal => {}
            Scenario::Stress => {
                // Clips of 900 contracts (about $67k) are over the $50k per-fill cap
                specs.push(Spec::new("mm-deep", Persona::MarketMaker, 250_000 * USDC, Strategy::mm(12.0, 900, 3_000)));
                let whale = Strategy::retail(Persona::Whale, rng);
                specs.push(Spec::new("whale", Persona::Whale, 60_000 * USDC, whale));
                for name in ["gapper-1", "gapper-2"] {
                    let s = Strategy::retail(Persona::Gapper, rng);
                    specs.push(Spec::new(name, Persona::Gapper, rng.gen_range(5_000..=15_000) * USDC, s));
                }
                for name in ["limit-big-1", "limit-big-2"] {
                    let s = Strategy::retail(Persona::LimitBig, rng);
                    specs.push(Spec::new(name, Persona::LimitBig, 150_000 * USDC, s));
                }
            }
            Scenario::Adversarial => {
                for name in ["colluder-a", "colluder-b"] {
                    specs.push(Spec::new(name, Persona::Colluder, 40_000 * USDC, Strategy::mm(0.0, 0, 0)));
                }
                specs.push(Spec::new("prober", Persona::Prober, 20_000 * USDC, Strategy::mm(0.0, 0, 0)));
                // The probes' own wallets (adversary.rs): a pair carried over a
                // forced gap, and a pair that self-crosses to push the net
                for name in ["gap-a", "gap-b", "cross-a", "cross-b"] {
                    specs.push(Spec::new(name, Persona::Prober, 400_000 * USDC, Strategy::mm(0.0, 0, 0)));
                }
            }
        }
        specs
    }

    async fn add_agent(&mut self, spec: Spec) {
        let base = self.chain.base();
        let t = trader(base, spec.usdc).await;
        let mut minted = spec.usdc;
        let mut mm = None;
        let now = now(base).await;
        if spec.registers_mm {
            // Registration bond in mm-registry, separate from perps-core margin
            let bond_amount = 1_000 * USDC;
            mint_to(base, t.tokens, bond_amount).await;
            minted += bond_amount;
            let market_maker = pda(&[b"market_maker", self.registry.as_ref(), t.key().as_ref()], &mm_registry::ID);
            let bond = pda(&[b"mm_collateral", market_maker.as_ref()], &mm_registry::ID);
            let accounts = mm_registry::accounts::RegisterMm {
                owner: t.key(), registry: self.registry, market_maker, mm_collateral_account: bond, owner_token_account: t.tokens,
                collateral_mint: base.mint, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
            };
            send(&mut base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::RegisterMm { initial_collateral: bond_amount })], &[&t.kp]).await.unwrap();
            // Maker approval: the risk budget, a 30 bps adverse-deviation bound, both sides
            let budget = spec.usdc;
            let mut params = maker_approval(budget, now);
            if spec.persona == Persona::Colluder {
                // It quotes at the band edge on purpose: no adverse bound
                params.max_price_deviation_bps = 0;
            }
            let approval = approve(base, &t.kp, &self.mm_venue, ROLE_MAKER, params).await.unwrap();
            self.ledger.track_approval(approval, budget);
            mm = Some(MmBook::new(market_maker, bond, budget));
        }
        if spec.approves_book {
            // Resting orders can end up either taker or maker: approve both roles
            for role in [ROLE_TAKER, ROLE_MAKER] {
                let params = VenueApprovalParams { expires_at: now + 30 * 86_400, ..approval_params(spec.usdc, now) };
                let approval = approve(base, &t.kp, &self.book_venue, role, params).await.unwrap();
                self.ledger.track_approval(approval, spec.usdc);
            }
        }
        self.total_minted += minted;
        self.agents.push(Agent::new(spec.name, spec.persona, t, minted, spec.usdc, spec.strategy, mm));
    }

    // --- chain reads ------------------------------------------------------------

    pub async fn market(&mut self) -> Market {
        market(self.chain.base()).await
    }

    pub async fn pool(&mut self) -> LpPool {
        let address = self.chain.base.lp_pool;
        load(self.chain.base(), address).await
    }

    /// The market and its pool, as the model takes them.
    pub async fn st(&mut self) -> crate::model::St {
        crate::model::St { m: self.market().await, pool: self.pool().await }
    }

    pub async fn lp_position(&mut self, lp: usize) -> LpPosition {
        let address = self.lps[lp].lp.position;
        load(self.chain.base(), address).await
    }

    pub async fn user_of(&mut self, agent: usize) -> UserAccount {
        let u = self.agents[agent].t.user;
        load(self.chain.base(), u).await
    }

    pub async fn wallet(&mut self, agent: usize) -> u64 {
        let tokens = self.agents[agent].t.tokens;
        token_balance(self.chain.base(), tokens).await
    }

    /// The session by the sim's own schedule model (the market runs CME Globex hours).
    pub fn in_session(&self) -> bool {
        crate::schedule::is_open(self.now)
    }

    /// The oracle as last published.
    pub fn ov(&self) -> OracleView {
        OracleView { price: self.px, conf: self.conf, published_at: self.published_at }
    }

    /// Whether the oracle passes the engine's checks (venue age `max_age`) now.
    pub fn oracle_ok(&self, m: &Market, max_age: u64) -> bool {
        crate::predict::oracle(m, &self.ov(), self.now, max_age).is_none()
    }

    /// Count an engine path or rejection reached, for the coverage report.
    pub fn hit(&mut self, path: impl Into<String>) {
        *self.metrics.paths.entry(path.into()).or_default() += 1;
    }

    pub fn hit_rejection(&mut self, r: Rej) {
        self.hit(format!("reject.{}", r.1));
    }

    pub fn nonce(&mut self) -> u64 {
        self.rng.gen()
    }

    // --- perps-core instructions not in the harness as builders ---------------------

    pub fn deposit_ix(&self, agent: usize, amount: u64) -> Instruction {
        let t = &self.agents[agent].t;
        let b = &self.chain.base;
        let accounts = perps_core::accounts::DepositCollateral {
            owner: t.key(), user_account: t.user, market: b.market, vault: b.vault,
            vault_token_account: b.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
            lp_pool: b.lp_pool,
        };
        ix(perps_core::ID, accounts, perps_core::instruction::DepositCollateral { amount })
    }

    pub fn withdraw_ix(&self, agent: usize, amount: u64) -> Instruction {
        let t = &self.agents[agent].t;
        let b = &self.chain.base;
        let accounts = perps_core::accounts::WithdrawCollateral {
            owner: t.key(), user_account: t.user, market: b.market, vault: b.vault,
            vault_token_account: b.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
            lp_pool: b.lp_pool,
        };
        ix(perps_core::ID, accounts, perps_core::instruction::WithdrawCollateral { amount })
    }

    /// liquidate by `liquidator`, its reward to `reward_to` (the keeper's wallet,
    /// or the owner's own in the self-liquidation probe).
    pub fn liquidate_ix(&self, liquidator: &Pubkey, reward_to: Pubkey, owner: &Pubkey, position: Pubkey) -> Instruction {
        let b = &self.chain.base;
        let accounts = perps_core::accounts::Liquidate {
            liquidator: *liquidator, position_owner: *owner, user_account: user_pda(owner, &b.market), market: b.market,
            position, vault: b.vault, vault_token_account: b.vault_token, liquidator_token_account: reward_to,
            pyth_price_feed: b.oracle, token_program: spl_token::ID, lp_pool: b.lp_pool,
        };
        ix(perps_core::ID, accounts, perps_core::instruction::Liquidate {})
    }

    pub fn settle_pnl_ix(&self, position: Pubkey) -> Instruction {
        settle_pnl_ix(&self.chain.base, &self.keeper.key(), position)
    }

    /// adl by `signer` (the keeper for mode 0; the admin, the pool's adl_keeper, for mode 1).
    pub fn adl_ix(&self, signer: &Pubkey, owner: &Pubkey, position: Pubkey, mode: u8, size: u64) -> Instruction {
        adl_ix(&self.chain.base, signer, owner, position, mode, size)
    }

    pub fn close_settled_ix(&self, owner: &Pubkey, position: Pubkey) -> Instruction {
        let b = &self.chain.base;
        let accounts = perps_core::accounts::CloseSettledPosition {
            signer: self.keeper.key(), market: b.market, lp_pool: b.lp_pool, position, position_owner: *owner, user_account: user_pda(owner, &b.market),
        };
        ix(perps_core::ID, accounts, perps_core::instruction::CloseSettledPosition {})
    }

    pub fn sweep_ix(&self) -> Instruction {
        let b = &self.chain.base;
        let accounts = perps_core::accounts::SweepInsuranceExcess { signer: self.keeper.key(), market: b.market, lp_pool: b.lp_pool };
        ix(perps_core::ID, accounts, perps_core::instruction::SweepInsuranceExcess {})
    }

    pub fn write_off_ix(&self) -> Instruction {
        let b = &self.chain.base;
        let accounts = perps_core::accounts::WriteOffBadDebt { signer: self.keeper.key(), market: b.market, lp_pool: b.lp_pool };
        ix(perps_core::ID, accounts, perps_core::instruction::WriteOffBadDebt {})
    }

    pub fn update_funding_ix(&self) -> Instruction {
        let b = &self.chain.base;
        let accounts = perps_core::accounts::UpdateFunding { keeper: self.keeper.key(), market: b.market, pyth_price_feed: b.oracle };
        ix(perps_core::ID, accounts, perps_core::instruction::UpdateFunding {})
    }

    // --- mm-registry ------------------------------------------------------------------

    pub fn market_maker_pda(&self, owner: &Pubkey) -> Pubkey {
        pda(&[b"market_maker", self.registry.as_ref(), owner.as_ref()], &mm_registry::ID)
    }

    pub fn post_quote_ix(&self, owner: &Pubkey, quote_index: u64, params: PostQuoteParams) -> (Instruction, Pubkey) {
        let market_maker = self.market_maker_pda(owner);
        let quote = pda(&[b"quote", market_maker.as_ref(), &quote_index.to_le_bytes()], &mm_registry::ID);
        let accounts = mm_registry::accounts::PostQuote { owner: *owner, registry: self.registry, market_maker, quote, system_program: system_program::ID };
        (ix(mm_registry::ID, accounts, mm_registry::instruction::PostQuote { params }), quote)
    }

    pub fn update_quote_ix(&self, owner: &Pubkey, quote: Pubkey, params: UpdateQuoteParams) -> Instruction {
        let accounts = mm_registry::accounts::UpdateQuote { owner: *owner, registry: self.registry, market_maker: self.market_maker_pda(owner), quote };
        ix(mm_registry::ID, accounts, mm_registry::instruction::UpdateQuote { params })
    }

    pub fn cancel_quote_ix(&self, owner: &Pubkey, quote: Pubkey) -> Instruction {
        let accounts = mm_registry::accounts::CancelQuote { owner: *owner, registry: self.registry, market_maker: self.market_maker_pda(owner), quote };
        ix(mm_registry::ID, accounts, mm_registry::instruction::CancelQuote {})
    }

    pub fn deregister_ix(&self, agent: usize) -> Instruction {
        let t = &self.agents[agent].t;
        let mm = self.agents[agent].mm.as_ref().expect("a market maker");
        let accounts = mm_registry::accounts::DeregisterMm {
            owner: t.key(), registry: self.registry, market_maker: mm.market_maker, mm_collateral_account: mm.bond,
            owner_token_account: t.tokens, token_program: spl_token::ID,
        };
        ix(mm_registry::ID, accounts, mm_registry::instruction::DeregisterMm {})
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_quote_ix(&self, taker: &Pubkey, mm: &Pubkey, quote: Pubkey, size: u64, is_buy: bool, limit: u64, leverage: u32, nonce: u64) -> (Instruction, FillAccounts) {
        let fa = fill_accounts(&self.chain.base, &mm_registry::ID, taker, mm, nonce);
        let accounts = mm_registry::accounts::FillQuote {
            taker: *taker, registry: self.registry, market_maker: self.market_maker_pda(mm), quote, mm_owner: fa.maker,
            venue_authority: fa.venue_authority, perps_config: fa.config, perps_venue: fa.venue,
            venue_program_data: fa.venue_program_data, market: fa.market, pyth_price_feed: fa.pyth_price_feed,
            taker_user: fa.taker_user, taker_approval: fa.taker_approval, taker_position: fa.taker_position,
            mm_user: fa.maker_user, mm_approval: fa.maker_approval, mm_position: fa.maker_position, perps_lp_pool: fa.lp_pool,
            perps_core_program: perps_core::ID, system_program: system_program::ID,
        };
        let params = mm_registry::instructions::FillQuoteParams {
            size, is_buy, limit_price: limit, taker_leverage: leverage, allow_partial: true, position_nonce: nonce,
        };
        (ix(mm_registry::ID, accounts, mm_registry::instruction::FillQuote { params }), fa)
    }

    // --- orderbook ----------------------------------------------------------------------

    pub fn place_order_ix(&self, owner: &Pubkey, sequence: u64, params: PlaceOrderParams) -> (Instruction, Pubkey) {
        let order = pda(&[b"order", self.book.as_ref(), owner.as_ref(), &sequence.to_le_bytes()], &orderbook::ID);
        let accounts = orderbook::accounts::PlaceOrder {
            owner: *owner, orderbook: self.book, market: self.chain.base.market, order, system_program: system_program::ID,
        };
        (ix(orderbook::ID, accounts, orderbook::instruction::PlaceOrder { params }), order)
    }

    pub fn cancel_order_ix(&self, owner: &Pubkey, order: Pubkey) -> Instruction {
        let accounts = orderbook::accounts::CancelOrder { owner: *owner, orderbook: self.book, order };
        ix(orderbook::ID, accounts, orderbook::instruction::CancelOrder {})
    }

    pub fn close_expired_ix(&self, owner: &Pubkey, order: Pubkey) -> Instruction {
        let accounts = orderbook::accounts::CloseExpiredOrder { orderbook: self.book, order, owner: *owner };
        ix(orderbook::ID, accounts, orderbook::instruction::CloseExpiredOrder {})
    }

    /// match_orders as the keeper builds it: the later order is the taker.
    pub fn match_ix(&self, bid: (&Pubkey, &orderbook::Order), ask: (&Pubkey, &orderbook::Order), nonce: u64, max_fill_size: u64) -> (Instruction, FillAccounts) {
        let (b, a) = (bid.1, ask.1);
        let (taker, maker) = if b.sequence_number > a.sequence_number { (b.owner, a.owner) } else { (a.owner, b.owner) };
        let fa = fill_accounts(&self.chain.base, &orderbook::ID, &taker, &maker, nonce);
        let accounts = orderbook::accounts::MatchOrders {
            matcher: self.keeper.key(),
            config: config_pda(),
            orderbook: self.book,
            bid_order: *bid.0,
            ask_order: *ask.0,
            bid_owner: b.owner,
            ask_owner: a.owner,
            venue_authority: fa.venue_authority,
            perps_venue: fa.venue,
            venue_program_data: fa.venue_program_data,
            market: fa.market,
            pyth_price_feed: fa.pyth_price_feed,
            taker_user: fa.taker_user,
            taker_approval: fa.taker_approval,
            taker_position: fa.taker_position,
            maker_user: fa.maker_user,
            maker_approval: fa.maker_approval,
            maker_position: fa.maker_position,
            perps_lp_pool: fa.lp_pool,
            perps_core_program: perps_core::ID,
            system_program: system_program::ID,
        };
        (ix(orderbook::ID, accounts, orderbook::instruction::MatchOrders { position_nonce: nonce, max_fill_size }), fa)
    }
}

/// An MM's standing maker approval: its risk budget, a 30 bps adverse bound,
/// both sides, 30 days.
pub fn maker_approval(budget: u64, now: i64) -> VenueApprovalParams {
    VenueApprovalParams { max_price_deviation_bps: 30, expires_at: now + 30 * 86_400, ..approval_params(budget, now) }
}
