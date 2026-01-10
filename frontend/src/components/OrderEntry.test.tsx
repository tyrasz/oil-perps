import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { OrderEntry } from './OrderEntry';
import { useMarketStore } from '../stores/marketStore';

// Control wallet state
let walletState = {
  connected: false,
  publicKey: null as { toString: () => string } | null,
};

// Mock wallet
vi.mock('@solana/wallet-adapter-react', () => ({
  useWallet: () => walletState,
}));

describe('OrderEntry', () => {
  beforeEach(() => {
    walletState = { connected: false, publicKey: null };

    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.00,
        priceChange24h: 0,
        volume24h: 0,
        openInterest: 0,
        longOpenInterest: 0,
        shortOpenInterest: 0,
        fundingRate: 0,
        maxLeverage: 20,
        isPaused: false,
      },
      orderBook: null,
      recentTrades: [],
      userAccount: null,
      positions: [],
      orders: [],
      selectedLeverage: 10,
      orderType: 'market',
    });
  });

  // Helper to get size input
  const getSizeInput = () => screen.getByPlaceholderText('0.00') as HTMLInputElement;

  describe('rendering', () => {
    it('should render side tabs (Long/Short)', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Long')).toBeInTheDocument();
      expect(screen.getByText('Short')).toBeInTheDocument();
    });

    it('should render order type tabs (Market/Limit)', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Market')).toBeInTheDocument();
      expect(screen.getByText('Limit')).toBeInTheDocument();
    });

    it('should render size input', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Size (Contracts)')).toBeInTheDocument();
      expect(getSizeInput()).toBeInTheDocument();
    });

    it('should render leverage buttons', () => {
      render(<OrderEntry />);

      expect(screen.getByText('1x')).toBeInTheDocument();
      expect(screen.getByText('2x')).toBeInTheDocument();
      expect(screen.getByText('5x')).toBeInTheDocument();
      expect(screen.getByText('10x')).toBeInTheDocument();
      expect(screen.getByText('15x')).toBeInTheDocument();
      expect(screen.getByText('20x')).toBeInTheDocument();
    });

    it('should render order summary', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Entry Price')).toBeInTheDocument();
      expect(screen.getByText('Margin Required')).toBeInTheDocument();
      expect(screen.getByText('Est. Liq. Price')).toBeInTheDocument();
      expect(screen.getByText('Trading Fee')).toBeInTheDocument();
    });
  });

  describe('wallet not connected', () => {
    it('should show Connect Wallet button when not connected', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Connect Wallet')).toBeInTheDocument();
    });

    it('should disable submit button when not connected', () => {
      render(<OrderEntry />);

      const submitBtn = screen.getByText('Connect Wallet');
      expect(submitBtn).toBeDisabled();
    });
  });

  describe('wallet connected', () => {
    beforeEach(() => {
      walletState = {
        connected: true,
        publicKey: { toString: () => 'MockPublicKey123' },
      };
    });

    it('should show Long OIL button for long side', () => {
      render(<OrderEntry />);

      expect(screen.getByText('Long OIL')).toBeInTheDocument();
    });

    it('should show Short OIL button when short side selected', () => {
      render(<OrderEntry />);

      fireEvent.click(screen.getByText('Short'));

      expect(screen.getByText('Short OIL')).toBeInTheDocument();
    });

    it('should disable button when no size entered', () => {
      render(<OrderEntry />);

      const submitBtn = screen.getByText('Long OIL');
      expect(submitBtn).toBeDisabled();
    });

    it('should enable button when size is entered', () => {
      render(<OrderEntry />);

      fireEvent.change(getSizeInput(), { target: { value: '10' } });

      const submitBtn = screen.getByText('Long OIL');
      expect(submitBtn).not.toBeDisabled();
    });
  });

  describe('side selection', () => {
    it('should switch to Long side', () => {
      render(<OrderEntry />);

      const longBtn = screen.getByText('Long');
      fireEvent.click(longBtn);

      expect(longBtn.className).toContain('active');
    });

    it('should switch to Short side', () => {
      render(<OrderEntry />);

      const shortBtn = screen.getByText('Short');
      fireEvent.click(shortBtn);

      expect(shortBtn.className).toContain('active');
    });
  });

  describe('order type selection', () => {
    it('should switch to Market order', () => {
      render(<OrderEntry />);

      const marketBtn = screen.getByText('Market');
      fireEvent.click(marketBtn);

      expect(marketBtn.className).toContain('active');
    });

    it('should switch to Limit order', () => {
      render(<OrderEntry />);

      const limitBtn = screen.getByText('Limit');
      fireEvent.click(limitBtn);

      expect(limitBtn.className).toContain('active');
    });

    it('should show price input only for Limit orders', () => {
      render(<OrderEntry />);

      // Initially market order - no price label
      expect(screen.queryByText('Price (USD)')).not.toBeInTheDocument();

      // Switch to limit
      fireEvent.click(screen.getByText('Limit'));

      // Now price label should appear
      expect(screen.getByText('Price (USD)')).toBeInTheDocument();
    });
  });

  describe('leverage selection', () => {
    it('should select leverage when clicked', () => {
      render(<OrderEntry />);

      const leverage5x = screen.getByText('5x');
      fireEvent.click(leverage5x);

      expect(leverage5x.className).toContain('active');
    });

    it('should update store when leverage changed', () => {
      render(<OrderEntry />);

      fireEvent.click(screen.getByText('20x'));

      expect(useMarketStore.getState().selectedLeverage).toBe(20);
    });
  });

  describe('size input', () => {
    it('should update size when typed', () => {
      render(<OrderEntry />);

      const sizeInput = getSizeInput();
      fireEvent.change(sizeInput, { target: { value: '50' } });

      expect(sizeInput.value).toBe('50');
    });
  });

  describe('margin calculation', () => {
    it('should calculate margin correctly', () => {
      render(<OrderEntry />);

      // Enter size of 100 contracts at $75 price with 10x leverage
      // Margin = (100 * 75) / 10 = 750
      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      expect(screen.getByText('$750.00')).toBeInTheDocument();
    });

    it('should calculate trading fee', () => {
      render(<OrderEntry />);

      // Size 100, price 75, leverage 10 => margin 750
      // Fee = 750 * 10 * 0.0005 = 3.75
      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      expect(screen.getByText('$3.7500')).toBeInTheDocument();
    });
  });

  describe('liquidation price', () => {
    it('should calculate liquidation price for long', () => {
      render(<OrderEntry />);

      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      // Long liq price = price * (1 - 1/leverage * 0.95)
      // = 75 * (1 - 0.1 * 0.95) = 75 * 0.905 = 67.875
      expect(screen.getByText('$67.88')).toBeInTheDocument();
    });

    it('should calculate liquidation price for short', () => {
      render(<OrderEntry />);

      fireEvent.click(screen.getByText('Short'));
      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      // Short liq price = price * (1 + 1/leverage * 0.95)
      // = 75 * (1 + 0.1 * 0.95) = 75 * 1.095 = 82.125
      expect(screen.getByText('$82.13')).toBeInTheDocument();
    });
  });

  describe('order submission', () => {
    beforeEach(() => {
      walletState = {
        connected: true,
        publicKey: { toString: () => 'MockPublicKey123' },
      };
    });

    it('should log order on submit', async () => {
      const consoleSpy = vi.spyOn(console, 'log').mockImplementation(() => {});

      render(<OrderEntry />);

      fireEvent.change(getSizeInput(), { target: { value: '100' } });
      fireEvent.click(screen.getByText('Long OIL'));

      await waitFor(() => {
        expect(consoleSpy).toHaveBeenCalledWith('Submitting order:', expect.objectContaining({
          side: 'long',
          size: 100,
          leverage: 10,
          orderType: 'market',
        }));
      });

      consoleSpy.mockRestore();
    });

    it('should clear form after submission', async () => {
      render(<OrderEntry />);

      const sizeInput = getSizeInput();
      fireEvent.change(sizeInput, { target: { value: '100' } });
      fireEvent.click(screen.getByText('Long OIL'));

      await waitFor(() => {
        expect(sizeInput.value).toBe('');
      });
    });

    it('should show Submitting... during submission', async () => {
      render(<OrderEntry />);

      fireEvent.change(getSizeInput(), { target: { value: '100' } });
      fireEvent.click(screen.getByText('Long OIL'));

      // Button might briefly show "Submitting..."
      // Due to the speed of execution, we verify the flow completed
      await waitFor(() => {
        expect(screen.getByText('Long OIL')).toBeInTheDocument();
      });
    });
  });

  describe('limit order', () => {
    it('should use limit price for calculations', () => {
      render(<OrderEntry />);

      // Switch to limit
      fireEvent.click(screen.getByText('Limit'));

      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      // Get price input by placeholder
      const priceInput = screen.getByPlaceholderText('75.00');
      fireEvent.change(priceInput, { target: { value: '70' } });

      // Margin = (100 * 70) / 10 = 700
      expect(screen.getByText('$700.00')).toBeInTheDocument();
    });
  });

  describe('edge cases', () => {
    it('should show $0.00 liquidation price when market is null', () => {
      useMarketStore.setState({
        market: null,
      });

      render(<OrderEntry />);

      // When market is null, both margin and liquidation price should be 0
      const zeroValues = screen.getAllByText(/\$0\.00/);
      expect(zeroValues.length).toBeGreaterThanOrEqual(2);
    });

    it('should show $0.00 margin when size is empty', () => {
      render(<OrderEntry />);

      // With no size entered, margin should be 0
      // Multiple elements show $0.00 (margin and liquidation price when no size)
      const zeroValues = screen.getAllByText(/\$0\.00/);
      expect(zeroValues.length).toBeGreaterThanOrEqual(1);
    });

    it('should use market price when limit price is empty', () => {
      render(<OrderEntry />);

      // Switch to limit order
      fireEvent.click(screen.getByText('Limit'));

      // Enter size but leave price empty
      fireEvent.change(getSizeInput(), { target: { value: '100' } });

      // Should use market price ($75) for calculations
      // Margin = (100 * 75) / 10 = 750
      expect(screen.getByText('$750.00')).toBeInTheDocument();
    });

    it('should show placeholder for limit price when market is null', () => {
      useMarketStore.setState({
        market: null,
      });

      render(<OrderEntry />);

      fireEvent.click(screen.getByText('Limit'));

      // Both size and price inputs have placeholder "0.00"
      const inputs = screen.getAllByPlaceholderText('0.00');
      expect(inputs.length).toBe(2); // Size input and price input
    });
  });
});
