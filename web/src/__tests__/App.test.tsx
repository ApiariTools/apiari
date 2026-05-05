import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('../api');

import App from '../App';

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(window, 'innerWidth', { value: 1200, writable: true });
  window.dispatchEvent(new Event('resize'));
});

describe('App shell', () => {
  it('renders the hive logo in sidebar', () => {
    render(<App />);
    expect(screen.getByText('hive')).toBeInTheDocument();
  });

  it('shows empty state when nothing is selected', () => {
    render(<App />);
    expect(screen.getByText('Select something to get started')).toBeInTheDocument();
  });

  it('shows Auto Bots section in sidebar', () => {
    render(<App />);
    expect(screen.getByText('Auto Bots')).toBeInTheDocument();
    expect(screen.getByText('Triage')).toBeInTheDocument();
    expect(screen.getByText('Standup')).toBeInTheDocument();
  });

  it('shows Workers section in sidebar', () => {
    render(<App />);
    expect(screen.getByText('Workers')).toBeInTheDocument();
    expect(screen.getByText('fix-auth')).toBeInTheDocument();
    expect(screen.getByText('rate-limit')).toBeInTheDocument();
    expect(screen.getByText('update-deps')).toBeInTheDocument();
  });

  it('shows placeholder detail when a worker is selected', async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByText('fix-auth'));
    expect(screen.getByText('Worker: fix-auth')).toBeInTheDocument();
  });

  it('shows placeholder detail when an auto bot is selected', async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByText('Triage'));
    expect(screen.getByText('Auto Bot: triage')).toBeInTheDocument();
  });
});

describe('Mobile layout', () => {
  it('shows bottom tab bar on mobile', () => {
    Object.defineProperty(window, 'innerWidth', { value: 375, writable: true });
    window.dispatchEvent(new Event('resize'));
    render(<App />);
    expect(screen.getByText('Auto Bots')).toBeInTheDocument();
    expect(screen.getByText('Workers')).toBeInTheDocument();
    expect(screen.getByText('Chat')).toBeInTheDocument();
    expect(screen.getByText('New')).toBeInTheDocument();
  });
});
