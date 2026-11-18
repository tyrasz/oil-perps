import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { SessionNotice } from './SessionNotice';

describe('SessionNotice', () => {
  it('renders nothing while the market is open', () => {
    const { container } = render(
      <SessionNotice session={{ state: 'open', secondsUntilClose: 86_400, reopens: null }} maxLeverage={10} />
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('warns a day ahead that weekend margins are coming', () => {
    const soon = { state: 'open' as const, secondsUntilClose: 30_000, secondsUntilWeekendMargins: 22_800, reopens: null };
    render(<SessionNotice session={soon} maxLeverage={10} weekendMaintenanceMarginRatio={1_000} />);
    expect(screen.getByText(/Weekend margins start in 6h 20m/)).toHaveTextContent(/below 10\.0% margin will become liquidatable/);
  });

  it('stays quiet more than a day before weekend margins', () => {
    const far = { state: 'open' as const, secondsUntilClose: 200_000, secondsUntilWeekendMargins: 192_800, reopens: null };
    const { container } = render(<SessionNotice session={far} maxLeverage={10} weekendMaintenanceMarginRatio={1_000} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('explains what is blocked while closed', () => {
    render(<SessionNotice session={{ state: 'closed', secondsUntilClose: null, reopens: 'Sunday 6:00 PM ET' }} maxLeverage={10} />);
    expect(screen.getByText(/Trading resumes Sunday 6:00 PM ET/)).toHaveTextContent(/adding margin still work/);
  });

  it('shows the weekend leverage cap and liquidation threshold', () => {
    render(
      <SessionNotice
        session={{ state: 'pre-close', secondsUntilClose: 5_400, reopens: null }}
        maxLeverage={5}
        weekendMaintenanceMarginRatio={1_000}
      />
    );
    expect(screen.getByText(/closes in 1h 30m/)).toHaveTextContent(
      'Max leverage is 5x, and positions below 10.0% margin can be liquidated.'
    );
  });

  it('shows the settlement price', () => {
    render(
      <SessionNotice session={{ state: 'settled', secondsUntilClose: null, reopens: null }} maxLeverage={10} settlementPrice={12_340_000} />
    );
    expect(screen.getByText(/Market settled at \$12\.34/)).toBeInTheDocument();
  });
});
