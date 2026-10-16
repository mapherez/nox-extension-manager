import '@testing-library/jest-dom/vitest';

// jsdom has no native modal lifecycle. Model the open state without changing
// production dialogs or substituting the browser's behavior in the app.
Object.defineProperty(HTMLDialogElement.prototype, 'showModal', {
  configurable: true,
  value(this: HTMLDialogElement) { this.setAttribute('open', ''); },
});
