import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ThemeProvider } from './contexts/ThemeContext';
import './i18n';
import './index.css';

const rootEl = document.getElementById('root');
if (!rootEl) {
  // Belt-and-suspenders: also check the body as a last resort so we
  // surface a clear error instead of dereferencing `null`.
  const fallback = document.body;
  if (!fallback) {
    throw new Error(
      'mAgent host UI: neither <div id="root"> nor <body> is present in index.html',
    );
  }
  // eslint-disable-next-line no-console
  console.error('[magent-man] #root element missing; falling back to <body>');
  ReactDOM.createRoot(fallback).render(
    <React.StrictMode>
      <ThemeProvider>
        <App />
      </ThemeProvider>
    </React.StrictMode>,
  );
} else {
  ReactDOM.createRoot(rootEl).render(
    <React.StrictMode>
      <ThemeProvider>
        <App />
      </ThemeProvider>
    </React.StrictMode>,
  );
}
