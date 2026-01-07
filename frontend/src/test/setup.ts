import '@testing-library/jest-dom';
import { vi } from 'vitest';

// Mock import.meta.env
vi.stubGlobal('import', {
  meta: {
    env: {
      VITE_API_URL: 'http://localhost:3001',
      VITE_WS_URL: 'ws://localhost:3001/ws',
    },
  },
});

// Mock WebSocket
class MockWebSocket {
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: ((error: Error) => void) | null = null;

  send = vi.fn();
  close = vi.fn();
}

vi.stubGlobal('WebSocket', MockWebSocket);

// Mock fetch
global.fetch = vi.fn();
