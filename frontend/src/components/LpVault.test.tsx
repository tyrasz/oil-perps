import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, screen, fireEvent } from '@testing-library/react';
import { decodeLpPool, decodeMarket, lpGates, poolView, productionPoolParams, withdrawalRequest } from '../utils/pool';
import { EMPTY_POOL_STATE, navPerShare, redeemValue } from '../utils/poolMath';

const C6 = 1_000_000n;
const WEDNESDAY = 1_794_416_400;
const SUNDAY_OPEN = 1_794_607_200 + 49 * 3_600;

const market = decodeMarket({
  initialMarginRatio: 1_000,
  maintenanceMarginRatio: 500,
  sessionSchedule: 1,
  fundingInterval: 3_600,
  insuranceFund: { toString: () => '5000000000' },
});
const pool = { ...decodeLpPool({}), ...EMPTY_POOL_STATE, cash: 1_000_000n * C6, lpShares: 1_000_000n * C6, params: productionPoolParams() };
const lpPosition = {
  shares: 10_000n * C6,
  pendingShares: 4_000n * C6,
  requestedValue: 3_900n * C6,
  requestedAt: BigInt(WEDNESDAY - 345_600),
  unlockAt: BigInt(WEDNESDAY - 7 * 86_400),
  depositedCum: 10_000n * C6,
  withdrawnCum: 0n,
  claimWindowSecs: 259_200n,
};

const deposit = vi.fn(async () => 'sig');
const requestWithdrawal = vi.fn(async () => 'sig');
const cancelWithdrawal = vi.fn(async () => 'sig');
const withdraw = vi.fn(async () => 'sig');
let state: Record<string, unknown> = {};

function vaultAt(now: number, m = { ...market, lastFundingTime: BigInt(now - 60) }) {
  const view = poolView(pool, m, 75n * C6, now);
  return {
    connected: true,
    market: m,
    pool,
    view,
    gates: lpGates(m, pool, view, now),
    lpPosition,
    request: withdrawalRequest(m, pool, lpPosition, view, now),
    summary: {
      navRedeemPerShare: navPerShare(view!.navRedeem, pool.lpShares),
      navMintPerShare: navPerShare(view!.navMint, pool.lpShares),
      positionValue: redeemValue(lpPosition.shares, pool.lpShares, view!.navRedeem),
      freeShares: lpPosition.shares - lpPosition.pendingShares,
    },
    now,
    isLoading: false,
    error: null,
    deposit,
    requestWithdrawal,
    cancelWithdrawal,
    withdraw,
  };
}

vi.mock('../hooks/useVault', () => ({ useVault: () => state }));

const { LpVault } = await import('./LpVault');

beforeEach(() => {
  vi.clearAllMocks();
});

describe('LpVault', () => {
  it('shows NAV per share, the pool and the lapsed request with cancel', () => {
    state = vaultAt(WEDNESDAY);
    render(<LpVault />);
    expect(screen.getByText('LP Pool · OIL')).toBeTruthy();
    expect(screen.getByText('Active')).toBeTruthy();
    // NAV_redeem per share just under $1 (1-unit slack)
    expect(screen.getByText('$0.999999')).toBeTruthy();
    expect(screen.getByText(/deposit at \$1.000000/)).toBeTruthy();
    // Unlocked a week ago: 115 open hours, past the 72-hour claim window
    expect(screen.getByText(/This request lapsed/)).toBeTruthy();
    expect(screen.queryByText(/^Claim \$/)).toBeNull();
    fireEvent.click(screen.getByText('Cancel request'));
    expect(cancelWithdrawal).toHaveBeenCalled();
  });

  it('explains the reopen quiet time and blocks the deposit', () => {
    state = vaultAt(SUNDAY_OPEN + 60);
    render(<LpVault />);
    fireEvent.change(screen.getByPlaceholderText('Amount to deposit'), { target: { value: '100' } });
    expect(screen.getByText(/closed for a short time after each session open \(30m 0s\)/)).toBeTruthy();
    expect((screen.getByText('Deposit USDC') as HTMLButtonElement).disabled).toBe(true);
  });

  it('quotes shares for a deposit and sends it', async () => {
    state = vaultAt(WEDNESDAY);
    render(<LpVault />);
    fireEvent.change(screen.getByPlaceholderText('Amount to deposit'), { target: { value: '100' } });
    expect(screen.getByText(/99\.9999\d* shares/)).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByText('Deposit USDC'));
    });
    expect(deposit).toHaveBeenCalledWith(100);
  });

  it('offers the claim once unlocked and in its window', () => {
    const fresh = { ...lpPosition, unlockAt: BigInt(WEDNESDAY - 60) };
    const s = vaultAt(WEDNESDAY);
    state = { ...s, lpPosition: fresh, request: withdrawalRequest(s.market, pool, fresh, s.view, WEDNESDAY) };
    render(<LpVault />);
    fireEvent.click(screen.getByText('Claim $3,900.00'));
    expect(withdraw).toHaveBeenCalled();
  });
});
