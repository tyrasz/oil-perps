import { formatDuration, WEEKEND_MARGIN_NOTICE_SECS, type MarketSession } from '../utils/session';
import { PRICE_DECIMALS } from '../config/program';

interface Props {
  session: MarketSession;
  /** Leverage cap in force (already lowered for weekend margins). */
  maxLeverage: number;
  weekendMaintenanceMarginRatio?: number;
  settlementPrice?: number;
}

/** Explains trading-hour, weekend-margin and settlement restrictions. */
export function SessionNotice({ session, maxLeverage, weekendMaintenanceMarginRatio = 0, settlementPrice = 0 }: Props) {
  switch (session.state) {
    case 'closed':
      return (
        <div className="warning-message session-notice">
          Market closed. Trading resumes {session.reopens}. Positions can't be opened, closed or
          liquidated until then; deposits and adding margin still work.
        </div>
      );
    case 'pre-close':
      return (
        <div className="warning-message session-notice">
          Weekend margins in force: market closes in {formatDuration(session.secondsUntilClose ?? 0)}.
          Max leverage is {maxLeverage}x
          {weekendMaintenanceMarginRatio > 0 &&
            `, and positions below ${(weekendMaintenanceMarginRatio / 100).toFixed(1)}% margin can be liquidated`}
          .
        </div>
      );
    case 'settled':
      return (
        <div className="error-message session-notice">
          Market settled at ${(settlementPrice / 10 ** PRICE_DECIMALS).toFixed(2)}. Positions can only be closed.
        </div>
      );
    default: {
      const until = session.secondsUntilWeekendMargins;
      if (until == null || until > WEEKEND_MARGIN_NOTICE_SECS || weekendMaintenanceMarginRatio <= 0) return null;
      return (
        <div className="warning-message session-notice">
          Weekend margins start in {formatDuration(until)}: positions below{' '}
          {(weekendMaintenanceMarginRatio / 100).toFixed(1)}% margin will become liquidatable. Add margin or
          reduce leverage before then.
        </div>
      );
    }
  }
}
