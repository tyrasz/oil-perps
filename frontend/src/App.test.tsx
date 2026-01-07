import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import App from './App';

describe('App', () => {
  beforeEach(() => {
    // Clear any previous renders
  });

  it('should render Vite + React heading', () => {
    render(<App />);

    expect(screen.getByText('Vite + React')).toBeInTheDocument();
  });

  it('should render Vite logo link', () => {
    render(<App />);

    const viteLink = screen.getByRole('link', { name: /vite logo/i });
    expect(viteLink).toHaveAttribute('href', 'https://vite.dev');
    expect(viteLink).toHaveAttribute('target', '_blank');
  });

  it('should render React logo link', () => {
    render(<App />);

    const reactLink = screen.getByRole('link', { name: /react logo/i });
    expect(reactLink).toHaveAttribute('href', 'https://react.dev');
    expect(reactLink).toHaveAttribute('target', '_blank');
  });

  it('should render count button with initial value of 0', () => {
    render(<App />);

    expect(screen.getByText('count is 0')).toBeInTheDocument();
  });

  it('should increment count when button is clicked', () => {
    render(<App />);

    const button = screen.getByText('count is 0');
    fireEvent.click(button);

    expect(screen.getByText('count is 1')).toBeInTheDocument();
  });

  it('should increment count multiple times', () => {
    render(<App />);

    const button = screen.getByText('count is 0');
    fireEvent.click(button);
    fireEvent.click(button);
    fireEvent.click(button);

    expect(screen.getByText('count is 3')).toBeInTheDocument();
  });

  it('should render HMR instruction text', () => {
    render(<App />);

    expect(screen.getByText(/Edit/)).toBeInTheDocument();
    expect(screen.getByText(/src\/App.tsx/)).toBeInTheDocument();
  });

  it('should render learn more text', () => {
    render(<App />);

    expect(screen.getByText(/Click on the Vite and React logos to learn more/)).toBeInTheDocument();
  });

  it('should render Vite logo image', () => {
    render(<App />);

    const viteLogo = screen.getByAltText('Vite logo');
    expect(viteLogo).toBeInTheDocument();
    expect(viteLogo.tagName).toBe('IMG');
  });

  it('should render React logo image', () => {
    render(<App />);

    const reactLogo = screen.getByAltText('React logo');
    expect(reactLogo).toBeInTheDocument();
    expect(reactLogo.tagName).toBe('IMG');
  });
});
