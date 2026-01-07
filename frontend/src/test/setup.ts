import '@testing-library/jest-dom';
import { vi } from 'vitest';
import React from 'react';

// Mock fetch
global.fetch = vi.fn(() =>
  Promise.resolve({
    json: () => Promise.resolve({}),
  })
) as any;

// Mock WebSocket
class MockWebSocket {
  static instances: MockWebSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: ((error: Error) => void) | null = null;

  constructor() {
    MockWebSocket.instances.push(this);
  }

  send = vi.fn();
  close = vi.fn();
}

vi.stubGlobal('WebSocket', MockWebSocket);

// Mock Solana wallet adapter
vi.mock('@solana/wallet-adapter-react', () => ({
  useConnection: () => ({
    connection: {
      getAccountInfo: vi.fn(),
      getBalance: vi.fn(),
    },
  }),
  useWallet: () => ({
    publicKey: null,
    connected: false,
    connecting: false,
    disconnect: vi.fn(),
    connect: vi.fn(),
    select: vi.fn(),
    wallet: null,
    wallets: [],
    signTransaction: vi.fn(),
    signAllTransactions: vi.fn(),
  }),
  ConnectionProvider: ({ children }: { children: React.ReactNode }) => children,
  WalletProvider: ({ children }: { children: React.ReactNode }) => children,
}));

// Mock Solana wallet adapter UI
vi.mock('@solana/wallet-adapter-react-ui', () => ({
  WalletModalProvider: ({ children }: { children: React.ReactNode }) => children,
  WalletMultiButton: () => React.createElement('button', { 'data-testid': 'wallet-button' }, 'Connect Wallet'),
}));

// Export mock utilities
export { MockWebSocket };
