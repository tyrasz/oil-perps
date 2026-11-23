// perps-core program errors (programs/perps-core/src/errors.rs), in enum
// order: Anchor numbers them from 6000. errors.test.ts checks this list
// against the program source.

/** `[name, program message]`, index = code - 6000. */
export const PERPS_ERRORS: [string, string][] = [
  ['MarketPaused', 'Market is currently paused'],
  ['ExcessiveLeverage', 'Leverage exceeds maximum allowed'],
  ['InsufficientCollateral', 'Insufficient collateral for position'],
  ['PositionTooSmall', 'Position size is too small'],
  ['PositionTooLarge', 'Position size exceeds maximum'],
  ['OpenInterestCapExceeded', 'Open interest cap would be exceeded'],
  ['BelowMaintenanceMargin', 'Position is below maintenance margin'],
  ['NotLiquidatable', 'Position is not liquidatable'],
  ['InvalidOraclePrice', 'Invalid oracle price'],
  ['StaleOraclePrice', 'Oracle price is stale'],
  ['InvalidPositionSide', 'Invalid position side'],
  ['PositionNotFound', 'Position not found'],
  ['PositionAlreadyClosed', 'Position is already closed'],
  ['Unauthorized', 'Unauthorized access'],
  ['MathOverflow', 'Math overflow'],
  ['InvalidMarketConfig', 'Invalid market configuration'],
  ['InvalidPositionReduction', 'Cannot reduce position below zero'],
  ['InsufficientVaultBalance', 'Insufficient vault balance'],
  ['InvalidReferralCode', 'Invalid referral code format'],
  ['ReferralCodeExists', 'Referral code already exists'],
  ['ReferralCodeNotFound', 'Referral code not found'],
  ['SelfReferralNotAllowed', 'Cannot use your own referral code'],
  ['ReferralAlreadyApplied', 'User already has a referral applied'],
  ['UserAlreadyHasCode', 'User already has a referral code'],
  ['NoRewardsToClaim', 'No rewards available to claim'],
  ['ReferralCodeInactive', 'Referral code is inactive'],
  ['InvalidReferralParams', 'Invalid referral parameters'],
  ['MarketClosed', 'Market is outside trading hours'],
  ['MarketSettled', 'Market has been settled; positions can only be closed'],
  ['OracleConfidenceTooWide', 'Oracle confidence interval is too wide'],
  ['OraclePriceBelowFloor', "Oracle price is below the market's minimum price"],
  ['InvalidOracleAccount', 'Price feed is not owned by the configured oracle program'],
  ['VenueUnauthorized', "Signer is not the venue's authority"],
  ['VenueDisabled', 'Venue is disabled'],
  ['VenueProgramChanged', 'Venue program was upgraded since it was registered; the admin must re-attest'],
  ['VenueFillsPaused', 'Venue fills are paused protocol-wide'],
  ['InvalidVenueParams', 'Invalid venue parameters'],
  ['InvalidFillParams', 'Invalid fill parameters'],
  ['FillPriceOutOfBand', "Fill price is outside the venue's band around the oracle"],
  ['FillTooLarge', "Fill notional exceeds the venue's per-fill cap"],
  ['SelfTrade', 'Taker and maker are the same wallet'],
  ['MissingSigner', 'Not enough of taker and maker signed'],
  ['ApprovalExpired', 'Venue approval has expired'],
  ['ApprovalAllowanceExceeded', 'Fill would commit more collateral than the approval allows'],
  ['ApprovalLeverageExceeded', "Leverage exceeds the approval's maximum"],
  ['ApprovalLimitPrice', "Fill price is beyond the approval's limit price"],
  ['ApprovalPriceDeviation', 'Fill price deviates too far from the oracle for the approval'],
  ['InvalidApprovalParams', 'Invalid venue approval parameters'],
  ['VenueMarginInvariant', 'Venue fill would open a position below initial margin'],
  ['ApprovalSideNotAllowed', 'The approval does not allow this side'],
  ['ApprovalViaCpi', 'Venue approvals can only be written by a top-level instruction'],
  ['InvalidPoolParams', 'Invalid LP pool parameters'],
  ['MarketNotFresh', 'Market or pool is not fresh'],
  ['PoolExposureLimit', "Trade would take the pool's net exposure past its cap"],
  ['PoolWeekendExposureLimit', "Trade would take the pool's net exposure past its weekend cap"],
  ['AccountExposureLimit', "Trade would take this account's house notional past its cap"],
  ['PoolAdlPending', 'Pool has pending bad debt awaiting ADL'],
  ['PoolCashInsufficient', 'Pool cash cannot cover this payout yet'],
  ['PoolWithdrawalsBlocked', 'LP withdrawals are blocked by the pool drawdown'],
  ['PoolCapWouldBreach', "Withdrawal would leave net exposure beyond the pool's hard line"],
  ['PoolCashCapExceeded', "Deposit would exceed the pool's capital cap"],
  ['PoolInsolvent', 'Pool NAV is not positive'],
  ['PoolHasNoLps', 'Pool has no LP capital yet'],
  ['SlippageExceeded', 'Price or shares are worse than the acceptable limit'],
  ['InvalidLpAmount', 'Invalid LP amount'],
  ['InvalidLpClass', 'Invalid LP share class'],
  ['InsufficientShares', 'Not enough free LP shares'],
  ['WithdrawalPending', 'A withdrawal request is already pending'],
  ['NoWithdrawalPending', 'No withdrawal request is pending'],
  ['WithdrawalDelayNotElapsed', 'Withdrawal delay has not elapsed'],
  ['NothingToSweep', 'No insurance excess to sweep'],
  ['InsufficientTreasury', 'Amount exceeds the treasury'],
  ['NothingToSettle', 'Position has no unrealized loss to settle'],
  ['SettleWouldBankrupt', "Settling would exhaust the position's collateral; liquidate instead"],
  ['InvalidAdlMode', 'Invalid ADL mode'],
  ['AdlUnauthorized', 'Only the ADL keeper or admin may run exposure ADL'],
  ['AdlNotRequired', 'ADL is not required'],
  ['AdlWrongSide', 'Position is on the wrong side for this ADL'],
  ['AdlCandidateNotProfitable', 'ADL candidate is not profitable'],
  ['AdlScoreBelowFloor', "ADL candidate's score is below the floor"],
  ['AdlOvershoot', 'ADL would reduce net exposure past its target'],
  ['BadDebtNotStale', 'Pending bad debt is not old enough to write off'],
  ['FundingNotDue', 'Funding is not due yet'],
  ['ParamsTimelocked', 'Pool parameter change is still timelocked'],
  ['NoPendingParams', 'No pool parameter change is pending'],
  ['BucketUnderflow', 'Pool bucket would go negative'],
  ['VaultInvariantViolated', 'Vault balance does not cover the pool buckets'],
  ['PoolDrawdownFreeze', 'Pool drawdown freezes net-raising opens'],
  ['LpFlowWindowClosed', 'LP flows are closed in this window'],
  ['FundingStale', 'Funding must be cranked first'],
  ['WithdrawalExpired', 'Withdrawal request has lapsed; cancel and request again'],
  ['MarketNotSettled', 'Market is not settled'],
  ['ProfitCloseQuiet', 'Profitable closes wait profit_close_quiet_secs after a session open'],
];

export const PERPS_ERROR_BASE = 6000;

export function errorName(code: number): string | undefined {
  return PERPS_ERRORS[code - PERPS_ERROR_BASE]?.[0];
}

export function errorCode(name: string): number | undefined {
  const i = PERPS_ERRORS.findIndex(([n]) => n === name);
  return i < 0 ? undefined : PERPS_ERROR_BASE + i;
}

/**
 * The perps-core error code carried by a failed RPC call, if any: an Anchor
 * `AnchorError` / `ProgramError`, or a raw "custom program error: 0x…" or
 * "Error Number: …" in the message or simulation logs.
 */
export function programErrorCode(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined;
  const err = error as Record<string, unknown>;
  const inner = err.error as { errorCode?: { number?: number } } | undefined;
  if (typeof inner?.errorCode?.number === 'number') return inner.errorCode.number;
  if (typeof err.code === 'number' && err.code >= PERPS_ERROR_BASE) return err.code;
  const texts: string[] = [];
  if (typeof err.message === 'string') texts.push(err.message);
  if (Array.isArray(err.logs)) texts.push(...(err.logs as unknown[]).filter((l): l is string => typeof l === 'string'));
  for (const t of texts) {
    const num = /Error Number: (\d+)/.exec(t);
    if (num) return Number(num[1]);
    const hex = /custom program error: 0x([0-9a-fA-F]+)/.exec(t);
    if (hex) return parseInt(hex[1], 16);
  }
  return undefined;
}

/** What the user was doing, so the same error can be explained for that action. */
export interface ErrorContext {
  action?: 'open' | 'close' | 'deposit' | 'withdraw' | 'margin' | 'lpDeposit' | 'lpRequest' | 'lpWithdraw' | 'lpCancel' | 'admin';
  /** ProfitCloseQuiet: seconds until a profitable close is accepted. */
  quietLeft?: number;
  /** LpFlowWindowClosed: which window is in force. */
  inWindow?: boolean;
  inQuiet?: boolean;
  /** LpFlowWindowClosed on a settled market: inside the settled quiet time (D41). */
  settledQuiet?: boolean;
  /** LpFlowWindowClosed: the reopen quiet time, seconds. */
  quietSecs?: number;
}

function formatWait(seconds: number): string {
  const s = Math.max(0, Math.ceil(seconds));
  const m = Math.floor(s / 60);
  return m > 0 ? `${m}m ${s % 60}s` : `${s}s`;
}

const lp = (a: ErrorContext['action']) => a === 'lpDeposit' || a === 'lpRequest' || a === 'lpWithdraw' || a === 'lpCancel';

/** A user-facing explanation of a program error, with what to do about it. */
export function explainProgramError(name: string, ctx: ErrorContext = {}): string {
  const a = ctx.action;
  switch (name) {
    case 'InsufficientCollateral':
      return 'Insufficient collateral. Please deposit more USDC first.';
    case 'StaleOraclePrice':
      return 'Oracle price is stale. Please try again.';
    case 'MarketClosed':
      return 'The market is outside trading hours. Try again when it reopens.';
    case 'SlippageExceeded':
      return a === 'lpDeposit'
        ? 'The pool NAV moved: this deposit would mint fewer shares than your minimum. Try again.'
        : 'The execution price moved past your slippage limit (skew impact or an oracle move). Try again, or allow more slippage.';
    case 'OpenInterestCapExceeded':
      return "This trade would take the market's open interest past its cap (counted in contracts per side).";
    case 'PoolExposureLimit':
      return a === 'close'
        ? "Closing this position would raise the LP pool's net exposure past its cap: its counterparty leg (a venue fill) is still open. " +
            'Exit by trading the opposite side on a venue (order book or market-maker quote), which leaves the pool net unchanged, and close both legs later; or wait for headroom.'
        : "This trade would take the LP pool's net exposure past its cap. Try a smaller size; trades on the other side are not limited.";
    case 'PoolWeekendExposureLimit':
      return a === 'close'
        ? "Closing this position would raise the LP pool's net exposure past its weekend cap, which applies before the weekly close. " +
            'Exit by trading the opposite side on a venue (order book or market-maker quote), which leaves the pool net unchanged, or close after the reopen.'
        : "This trade would take the LP pool's net exposure past its weekend cap, which applies before the weekly close. Try a smaller size, or the other side.";
    case 'AccountExposureLimit':
      return 'Your notional traded against the pool is at its per-account cap. Close some positions, or trade on a venue.';
    case 'PoolAdlPending':
      return lp(a)
        ? 'LP deposits and withdrawals are paused while the pool has pending bad debt awaiting auto-deleveraging. Withdrawal requests are still accepted.'
        : "The pool has pending bad debt awaiting auto-deleveraging: opens that raise its net exposure are paused. Closes and trades on the other side still work.";
    case 'PoolDrawdownFreeze':
      return "The pool's NAV is in drawdown past its freeze line: opens that raise its net exposure are paused. Closes and trades on the other side still work.";
    case 'PoolCashInsufficient':
      return lp(a)
        ? "The pool's cash cannot cover this withdrawal yet (its value is in open positions' unsettled losses). Try again later."
        : "The pool's cash cannot pay this profit yet. Keepers settle losing positions into the pool; try again shortly.";
    case 'ProfitCloseQuiet':
      return (
        'Profitable closes wait a few minutes after each session open (and after settlement) so bankrupt positions can be liquidated first.' +
        (ctx.quietLeft !== undefined && ctx.quietLeft > 0 ? ` Try again in ${formatWait(ctx.quietLeft)}.` : ' Try again shortly.')
      );
    case 'LpFlowWindowClosed': {
      const q = ctx.quietSecs ? ` (${formatWait(ctx.quietSecs)})` : '';
      if (ctx.settledQuiet) return 'After settlement, LP withdrawals wait a short quiet time so bankrupt positions are closed and their losses booked first. Try again shortly.';
      if (ctx.inWindow) return 'LP withdrawals are closed in the pre-close window before the weekly close. Claim after the market reopens.';
      if (ctx.inQuiet) return `LP deposits and withdrawals are closed for a short time after each session open${q}. Try again shortly.`;
      return `LP flows are closed right now: withdrawals in the pre-close window before the weekly close, deposits and withdrawals just after each session open${q}.`;
    }
    case 'FundingStale':
      return 'Funding must be cranked first. Try again: the app re-reads the market and cranks it first.';
    case 'FundingNotDue':
      return 'Funding is not due yet.';
    case 'WithdrawalExpired':
      return 'Your withdrawal request lapsed: its claim window passed. Cancel it and request again.';
    case 'WithdrawalDelayNotElapsed':
      return 'The withdrawal delay has not elapsed yet. Claim after the unlock time.';
    case 'WithdrawalPending':
      return 'You already have a withdrawal request pending. Claim or cancel it first.';
    case 'NoWithdrawalPending':
      return 'There is no withdrawal request to claim or cancel.';
    case 'InsufficientShares':
      return 'Not enough free LP shares (shares in a pending request are locked).';
    case 'PoolWithdrawalsBlocked':
      return "LP withdrawals are blocked while the pool's drawdown is past its block line.";
    case 'PoolCapWouldBreach':
      return "This withdrawal would leave the pool's net exposure beyond its hard line. Try a smaller amount, or wait until traders close or ADL reduces it.";
    case 'PoolCashCapExceeded':
      return 'The pool is at its capital cap; deposits are closed until it has room.';
    case 'PoolInsolvent':
      return "The pool's NAV is not positive; deposits are closed until it is recapitalised.";
    case 'PoolHasNoLps':
      return 'The LP pool has no capital yet; trading against the house opens after the seed deposit.';
    case 'ParamsTimelocked':
      return 'The proposed pool parameters are still timelocked.';
    case 'NoPendingParams':
      return 'No pool parameter change is pending.';
    case 'InvalidPoolParams':
      return 'The pool parameters are invalid for this market (PoolParams::validate).';
    case 'InsufficientTreasury':
      return 'The amount exceeds the treasury.';
    case 'VaultInvariantViolated':
      return a === 'admin' ? 'The destination cannot be the market vault.' : 'Vault balance does not cover the pool buckets.';
    default: {
      const i = PERPS_ERRORS.findIndex(([n]) => n === name);
      return i < 0 ? name : PERPS_ERRORS[i][1];
    }
  }
}

/**
 * A failed trade: `message` is for the user; `programError` is the perps-core
 * error name and `retryAfter` the seconds until a retry can succeed
 * (ProfitCloseQuiet), so automated closes can wait instead of giving up.
 */
export class TradeError extends Error {
  readonly programError?: string;
  readonly retryAfter?: number;
  constructor(message: string, programError?: string, retryAfter?: number) {
    super(message);
    this.name = 'TradeError';
    this.programError = programError;
    this.retryAfter = retryAfter;
  }
}

/** After a failed TP/SL close, wait this long before trying the order again. */
export const TRIGGER_RETRY_BACKOFF_MS = 10_000;

/**
 * When (ms) to retry a trigger order whose close failed: just after the
 * profit-close quiet time (D23) the error reports, else after a backoff.
 */
export function triggerRetryAt(error: unknown, nowMs: number): number {
  if (error instanceof TradeError && error.programError === 'ProfitCloseQuiet' && error.retryAfter !== undefined) {
    return nowMs + error.retryAfter * 1000 + 1000;
  }
  return nowMs + TRIGGER_RETRY_BACKOFF_MS;
}
