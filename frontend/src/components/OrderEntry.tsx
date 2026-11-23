import { useState, useCallback, useEffect, useMemo } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import { useTrading } from '../hooks/useTrading';
import { usePositions } from '../hooks/usePositions';
import { useOnChainMarket } from '../hooks/useOnChainMarket';
import type { OrderSide } from '../types';
import { estimateLiquidationPrice, formatLeverage, maxAllowedLeverage } from '../utils/leverage';
import { liquidationPrice } from '../utils/positionHealth';
import { useLpPool } from '../hooks/useLpPool';
import {
  acceptablePrice,
  checkOpenCaps,
  closeFee,
  fromUnits,
  houseQuote,
  marginRatios,
  openHeadroom,
  poolView,
  toUnits,
} from '../utils/pool';
import { explainProgramError } from '../utils/errors';
import { DEFAULT_SLIPPAGE_BPS } from '../config/program';
import { useMarketSession } from '../hooks/useMarketSession';
import { SessionNotice } from './SessionNotice';

// Maximum position size in USD value (per position)
const MAX_POSITION_VALUE = 100_000; // $100k max per position
const SLIPPAGE_OPTIONS_BPS = [10, DEFAULT_SLIPPAGE_BPS, 100];
const fmtUsd = (x: number) => x.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });

export function OrderEntry() {
  const { connected, publicKey } = useWallet();
  const {
    selectedCommodity,
    markets,
    selectedLeverage,
    orderType,
    setSelectedLeverage,
    setOrderType
  } = useMarketStore();
  const {
    openPosition,
    checkMarketExists,
    isLoading: isTradingLoading,
    error: tradingError,
    clearError,
    getExplorerUrl,
    lastTxSignature
  } = useTrading();
  const { userAccount } = usePositions();
  const { marketData: onChainMarket } = useOnChainMarket();
  const { data: poolData } = useLpPool(selectedCommodity.id);
  const [slippageBps, setSlippageBps] = useState(DEFAULT_SLIPPAGE_BPS);
  const session = useMarketSession(onChainMarket);
  const preClose = session.state === 'pre-close';
  const canOpen = session.state === 'open' || preClose;

  const market = markets[selectedCommodity.id];
  const availableCollateral = userAccount?.collateralBalance ?? 0;

  const [side, setSide] = useState<OrderSide>('long');
  const [size, setSize] = useState('');
  const [price, setPrice] = useState('');
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  // Answer is tagged with its commodity; an answer for another commodity reads as unknown
  const [marketCheck, setMarketCheck] = useState<{ id: string; exists: boolean } | null>(null);
  const marketExists = marketCheck?.id === selectedCommodity.id ? marketCheck.exists : null;

  // Check if market is initialized on-chain
  useEffect(() => {
    // Re-checked for each commodity (checkMarketExists changes with it)
    const id = selectedCommodity.id;
    if (connected) {
      checkMarketExists().then((exists) => setMarketCheck({ id, exists }));
    }
  }, [connected, checkMarketExists, selectedCommodity.id]);

  // Leverage options, capped by the commodity limit and the on-chain margin rules
  const maxLev = maxAllowedLeverage(selectedCommodity.maxLeverage, onChainMarket, preClose);
  const leverageOptions = [1, 2, 5, 10, 15, 20].filter(lev => lev <= maxLev);
  // The stored choice can exceed this market's cap (e.g. 10x selected, NATGAS
  // allows 6.67x); trade at the highest allowed option instead.
  const leverage = selectedLeverage <= maxLev ? selectedLeverage : leverageOptions[leverageOptions.length - 1] ?? 1;

  const calculateMargin = useCallback(() => {
    if (!size || !market) return 0;
    const sizeNum = parseFloat(size);
    const priceNum = orderType === 'market' ? market.price : parseFloat(price) || market.price;
    const notional = sizeNum * priceNum;
    return notional / leverage;
  }, [size, price, market, leverage, orderType]);

  // The house quote (docs/lp-pool.md §4.4): skew-impact execution price,
  // margin and open fee, from the market and pool accounts and the latest
  // price; plus how much the pool's caps still allow (§3).
  const nowSecs = Math.floor(Date.now() / 1000);
  const house = useMemo(() => {
    const sizeNum = parseFloat(size) || 0;
    if (!poolData?.pool || !market?.price) return null;
    const { market: m, pool } = poolData;
    const oracle = toUnits(market.price);
    const v = poolView(pool, m, oracle, nowSecs);
    if (!v) return null;
    const houseNotional = toUnits(userAccount?.houseEntryNotional ?? 0);
    const headroom = openHeadroom(m, pool, v, houseNotional, oracle, nowSecs);
    if (sizeNum <= 0) return { headroom, quote: null, capError: null };
    const sizeUnits = toUnits(sizeNum);
    const q = houseQuote(m, pool, side === 'long', sizeUnits, BigInt(Math.round(leverage * 1_000)), oracle, nowSecs);
    if (!q.ok) return { headroom, quote: null, capError: q.error };
    const capError = checkOpenCaps(m, pool, v, side === 'long', sizeUnits, oracle, houseNotional, q.value.notional, nowSecs);
    const [, mmr] = marginRatios(m, nowSecs);
    return {
      headroom,
      capError,
      quote: {
        ...q.value,
        execUsd: fromUnits(q.value.exec),
        marginUsd: fromUnits(q.value.margin),
        feeUsd: fromUnits(q.value.fee),
        closeFeeUsd: fromUnits(closeFee(sizeUnits, q.value.exec, m.takerFee)),
        liqPrice: liquidationPrice(
          { isLong: side === 'long', size: sizeNum, collateral: fromUnits(q.value.margin), entryPrice: fromUnits(q.value.exec) },
          Number(mmr)
        ),
        acceptable: acceptablePrice(side === 'long', q.value.exec, slippageBps),
      },
    };
    // nowSecs changes every render; the quote only needs refreshing with its inputs
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [poolData, market?.price, size, side, leverage, slippageBps, userAccount?.houseEntryNotional]);
  const quote = house?.quote ?? null;

  const handleSubmit = async () => {
    if (!connected || !publicKey || !size) return;

    // Clear previous messages
    clearError();
    setSuccessMessage(null);

    try {
      // Bounded at the quoted execution price, `slippageBps` worse
      await openPosition(side, parseFloat(size), leverage, { acceptablePrice: quote?.acceptable, slippageBps });

      // Reset form on success
      setSize('');
      setPrice('');
      setSuccessMessage(`Position opened! View on explorer`);

      // Auto-clear success message after 10 seconds
      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (error) {
      console.error('Failed to submit order:', error);
      // Error is already set in useTrading hook
    }
  };

  // Collateral the open moves out of the free balance: margin plus the open fee
  const margin = quote ? quote.marginUsd + quote.feeUsd : calculateMargin();
  const insufficientMargin = margin > 0 && margin > availableCollateral;
  const liqPrice = quote
    ? quote.liqPrice
    : market
    ? estimateLiquidationPrice(
        market.price,
        leverage,
        side,
        !onChainMarket
          ? undefined
          : preClose
          ? Math.max(onChainMarket.maintenanceMarginRatio, onChainMarket.weekendMaintenanceMarginRatio)
          : onChainMarket.maintenanceMarginRatio
      )
    : 0;

  // Position size validation
  const sizeValidation = useMemo(() => {
    const sizeNum = parseFloat(size) || 0;
    const priceNum = market?.price || 0;
    const notionalValue = sizeNum * priceNum;

    // Check minimum trade size
    if (sizeNum > 0 && sizeNum < selectedCommodity.minTradeSize) {
      return {
        valid: false,
        error: `Minimum size is ${selectedCommodity.minTradeSize} ${selectedCommodity.contractUnit}s`,
      };
    }

    // Check maximum position value
    if (notionalValue > MAX_POSITION_VALUE) {
      return {
        valid: false,
        error: `Max position value is $${MAX_POSITION_VALUE.toLocaleString()}`,
      };
    }

    // Check if would exceed max open interest (from on-chain); the cap is in
    // contracts per side (docs/lp-pool.md D6)
    if (onChainMarket && sizeNum > 0) {
      const currentOI = side === 'long'
        ? onChainMarket.longOpenInterest
        : onChainMarket.shortOpenInterest;
      const maxOI = onChainMarket.maxOpenInterest;

      if (currentOI + sizeNum > maxOI && maxOI > 0) {
        const available = Math.max(0, maxOI - currentOI);
        return {
          valid: false,
          error: `Exceeds max open interest. Available: ${available.toLocaleString()} ${selectedCommodity.contractUnit}s`,
        };
      }
    }

    // The pool's caps on house opens (net, weekend, per account) and freezes
    if (sizeNum > 0 && house?.capError) {
      return { valid: false, error: explainProgramError(house.capError, { action: 'open' }) };
    }

    // Check leverage doesn't exceed max
    if (leverage > maxLev) {
      return {
        valid: false,
        error: `Max leverage for ${selectedCommodity.id} is ${formatLeverage(maxLev)}x`,
      };
    }

    return { valid: true, error: null };
  }, [size, market, selectedCommodity, leverage, side, onChainMarket, maxLev, house?.capError]);

  const formatPrice = (value: number) => value.toFixed(selectedCommodity.decimals);

  return (
    <div className="order-entry">
      <div className="order-tabs">
        <button
          className={`tab ${side === 'long' ? 'active long' : ''}`}
          onClick={() => setSide('long')}
        >
          Long
        </button>
        <button
          className={`tab ${side === 'short' ? 'active short' : ''}`}
          onClick={() => setSide('short')}
        >
          Short
        </button>
      </div>

      <div className="order-type-tabs">
        <button
          className={`type-tab ${orderType === 'market' ? 'active' : ''}`}
          onClick={() => setOrderType('market')}
        >
          Market
        </button>
        <button
          className={`type-tab ${orderType === 'limit' ? 'active' : ''}`}
          onClick={() => setOrderType('limit')}
        >
          Limit
        </button>
      </div>

      <div className="form-group">
        <label>Size ({selectedCommodity.contractUnit}s)</label>
        <input
          type="number"
          value={size}
          onChange={(e) => setSize(e.target.value)}
          placeholder="0.00"
          min={selectedCommodity.minTradeSize}
          step={selectedCommodity.minTradeSize}
        />
      </div>

      {orderType === 'limit' && (
        <div className="form-group">
          <label>Price (USD)</label>
          <input
            type="number"
            value={price}
            onChange={(e) => setPrice(e.target.value)}
            placeholder={market?.price.toFixed(selectedCommodity.decimals) || '0.00'}
            min="0"
            step={selectedCommodity.tickSize}
          />
        </div>
      )}

      <div className="form-group">
        <label>Leverage</label>
        <div className="leverage-slider">
          {leverageOptions.map((lev) => (
            <button
              key={lev}
              className={`leverage-btn ${leverage === lev ? 'active' : ''}`}
              onClick={() => setSelectedLeverage(lev)}
            >
              {lev}x
            </button>
          ))}
        </div>
      </div>

      <div className="order-summary">
        <div className="summary-row">
          <span title="Execution price against the LP pool, including skew impact">Exec Price</span>
          <span>
            ${quote ? formatPrice(quote.execUsd) : orderType === 'market' ? formatPrice(market?.price || 0) : price || formatPrice(market?.price || 0)}
            {quote && quote.impactBps !== 0 && (
              <span className={quote.impactBps > 0 === (side === 'long') ? 'text-red' : 'text-green'}>
                {' '}({quote.impactBps > 0 ? '+' : ''}{quote.impactBps.toFixed(2)} bps impact)
              </span>
            )}
          </span>
        </div>
        <div className="summary-row">
          <span>Max Slippage</span>
          <span>
            {SLIPPAGE_OPTIONS_BPS.map((bps) => (
              <button
                key={bps}
                type="button"
                className={`leverage-btn ${slippageBps === bps ? 'active' : ''}`}
                onClick={() => setSlippageBps(bps)}
              >
                {bps / 100}%
              </button>
            ))}
          </span>
        </div>
        <div className="summary-row">
          <span>Available Collateral</span>
          <span>${availableCollateral.toFixed(2)}</span>
        </div>
        <div className="summary-row">
          <span title="Margin posted to the position plus the open fee">Margin Required</span>
          <span className={insufficientMargin ? 'text-red' : ''}>
            ${margin.toFixed(2)}
          </span>
        </div>
        <div className="summary-row">
          <span title="Liquidation compares equity with the maintenance margin on mark notional">Est. Liq. Price</span>
          <span className={side === 'long' ? 'text-red' : 'text-green'}>
            ${formatPrice(liqPrice)}
          </span>
        </div>
        {quote ? (
          <>
            <div className="summary-row">
              <span>Open Fee</span>
              <span>${quote.feeUsd.toFixed(4)}</span>
            </div>
            <div className="summary-row">
              <span title="Charged on close, on the opened notional">Close Fee</span>
              <span>${quote.closeFeeUsd.toFixed(4)}</span>
            </div>
          </>
        ) : (
          <div className="summary-row">
            <span>Trading Fee</span>
            <span>${(margin * leverage * 0.0005).toFixed(4)}</span>
          </div>
        )}
        {house && (
          <div className="summary-row" title="How much more the LP pool can take on this side before its caps bind">
            <span>Pool Headroom ({side === 'long' ? 'long' : 'short'})</span>
            <span>
              {house.headroom.frozen && house.headroom.frozen !== 'PoolHasNoLps' && (side === 'long' ? house.headroom.long : house.headroom.short) === 0n
                ? 'frozen'
                : `${fmtUsd(fromUnits(side === 'long' ? house.headroom.long : house.headroom.short))} ${selectedCommodity.contractUnit}s`}
              {house.headroom.netCap === 'weekend' && ' (weekend cap)'}
            </span>
          </div>
        )}
        {house?.headroom.account != null && (
          <div className="summary-row" title="Per-account cap on notional traded against the pool">
            <span>Your Headroom</span>
            <span>${fmtUsd(fromUnits(house.headroom.account))}</span>
          </div>
        )}
      </div>

      <SessionNotice
        session={session}
        maxLeverage={maxLev}
        maintenanceMarginRatio={onChainMarket?.maintenanceMarginRatio}
        weekendMaintenanceMarginRatio={onChainMarket?.weekendMaintenanceMarginRatio}
        settlementPrice={onChainMarket?.settlementPrice}
      />

      {/* Market not initialized warning */}
      {connected && marketExists === false && (
        <div className="error-message">
          Market not initialized on-chain. Run the initialize-market script first.
        </div>
      )}

      {/* Position size validation error */}
      {!sizeValidation.valid && sizeValidation.error && (
        <div className="warning-message">
          {sizeValidation.error}
        </div>
      )}

      {/* Insufficient margin warning */}
      {insufficientMargin && sizeValidation.valid && (
        <div className="warning-message">
          Insufficient collateral. Deposit ${(margin - availableCollateral).toFixed(2)} more or reduce position size.
        </div>
      )}

      {/* Error message */}
      {tradingError && (
        <div className="error-message" onClick={clearError}>
          {tradingError}
        </div>
      )}

      {/* Success message */}
      {successMessage && lastTxSignature && (
        <div className="success-message">
          {successMessage}{' '}
          <a
            href={getExplorerUrl(lastTxSignature)}
            target="_blank"
            rel="noopener noreferrer"
          >
            View tx
          </a>
        </div>
      )}

      <button
        className={`submit-btn ${side}`}
        onClick={handleSubmit}
        disabled={!connected || !size || isTradingLoading || insufficientMargin || marketExists === false || !sizeValidation.valid || !canOpen || (marketExists === true && !onChainMarket)}
      >
        {!connected
          ? 'Connect Wallet'
          : session.state === 'closed'
          ? 'Market Closed'
          : session.state === 'settled'
          ? 'Market Settled'
          : marketExists === false
          ? 'Market Not Initialized'
          : isTradingLoading
          ? 'Submitting...'
          : !sizeValidation.valid
          ? 'Invalid Size'
          : insufficientMargin
          ? 'Insufficient Collateral'
          : `${side === 'long' ? 'Long' : 'Short'} ${selectedCommodity.id}`}
      </button>
    </div>
  );
}
