// @vitest-environment node
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  PERPS_ERRORS,
  TRIGGER_RETRY_BACKOFF_MS,
  TradeError,
  errorCode,
  errorName,
  explainProgramError,
  programErrorCode,
  triggerRetryAt,
} from './errors';
import { parseAnchorError } from './transaction';

const ERRORS_RS = fileURLToPath(new URL('../../../programs/perps-core/src/errors.rs', import.meta.url));

describe('program error table', () => {
  it('lists the program errors in enum order, with their messages', () => {
    const src = readFileSync(ERRORS_RS, 'utf8');
    const variants = [...src.matchAll(/#\[msg\("((?:[^"\\]|\\.)*)"\)\]\s*(\w+),/g)].map((m) => [m[2], m[1]]);
    expect(variants.length).toBeGreaterThan(90);
    expect(PERPS_ERRORS).toEqual(variants);
  });

  it('maps codes and names', () => {
    expect(errorName(6000)).toBe('MarketPaused');
    expect(errorName(6092)).toBe('ProfitCloseQuiet');
    expect(errorCode('PoolExposureLimit')).toBe(6053);
    expect(errorName(5999)).toBeUndefined();
  });
});

describe('error extraction', () => {
  it('reads AnchorError, ProgramError, raw hex and logs', () => {
    expect(programErrorCode({ error: { errorCode: { code: 'ProfitCloseQuiet', number: 6092 } } })).toBe(6092);
    expect(programErrorCode({ code: 6053, msg: 'x' })).toBe(6053);
    expect(programErrorCode({ message: 'failed: custom program error: 0x17cc' })).toBe(6092);
    expect(programErrorCode({ logs: ['Program log: AnchorError ... Error Number: 6088. Error Message: x'] })).toBe(6088);
    expect(programErrorCode({ message: 'User rejected the request.' })).toBeUndefined();
    expect(programErrorCode(null)).toBeUndefined();
  });
});

describe('explanations', () => {
  it('explains a capped close with the venue-hedge exit', () => {
    expect(explainProgramError('PoolExposureLimit', { action: 'close' })).toMatch(/opposite side on a venue/);
    expect(explainProgramError('PoolWeekendExposureLimit', { action: 'close' })).toMatch(/weekend cap/);
    expect(explainProgramError('PoolExposureLimit', { action: 'open' })).not.toMatch(/venue/);
  });

  it('gives the time left in the profit-close quiet time', () => {
    expect(explainProgramError('ProfitCloseQuiet', { quietLeft: 125 })).toMatch(/Try again in 2m 5s/);
    expect(parseAnchorError({ code: 6092 }, { action: 'close', quietLeft: 30 })).toMatch(/30s/);
  });

  it('explains the LP-flow gates', () => {
    expect(explainProgramError('LpFlowWindowClosed', { inWindow: true })).toMatch(/pre-close window/);
    expect(explainProgramError('LpFlowWindowClosed', { inQuiet: true, quietSecs: 1_800 })).toMatch(/after each session open \(30m 0s\)/);
    expect(explainProgramError('LpFlowWindowClosed', { settledQuiet: true })).toMatch(/After settlement, LP withdrawals wait/);
    expect(explainProgramError('PoolAdlPending', { action: 'lpDeposit' })).toMatch(/requests are still accepted/);
    expect(explainProgramError('WithdrawalExpired')).toMatch(/Cancel it and request again/);
    expect(explainProgramError('FundingStale')).toMatch(/cranked/);
  });

  it('falls back to the program message, and keeps the wallet messages', () => {
    expect(explainProgramError('VenueDisabled')).toBe('Venue is disabled');
    expect(parseAnchorError({ code: 6002 })).toBe('Insufficient collateral. Please deposit more USDC first.');
    expect(parseAnchorError(new Error('User rejected the request.'))).toBe('Transaction was cancelled');
  });
});

describe('TP/SL retries', () => {
  it('waits out the profit-close quiet time, else backs off', () => {
    expect(triggerRetryAt(new TradeError('wait', 'ProfitCloseQuiet', 120), 1_000)).toBe(1_000 + 121_000);
    expect(triggerRetryAt(new TradeError('cap', 'PoolExposureLimit'), 1_000)).toBe(1_000 + TRIGGER_RETRY_BACKOFF_MS);
    expect(triggerRetryAt(new Error('rpc'), 0)).toBe(TRIGGER_RETRY_BACKOFF_MS);
  });
});
