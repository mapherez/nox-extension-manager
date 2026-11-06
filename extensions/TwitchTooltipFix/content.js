(() => {
  "use strict";

  const hoverAttribute = "data-nox-twitch-channel-hover";
  const channelSelector = [
    ".side-nav-card",
    '[data-test-selector="followed-channel"]',
    '[data-test-selector="recommended-channel"]'
  ].join(", ");

  const style = document.createElement("style");
  style.textContent = `
    html:not([${hoverAttribute}]) .tw-dialog-layer > *:has(
      .online-side-nav-channel-tooltip__body,
      .side-nav-guest-star-tooltip__body
    ) {
      display: none !important;
    }
  `;
  document.documentElement.appendChild(style);

  const hide = () => document.documentElement.removeAttribute(hoverAttribute);
  const hoveredCard = (target) =>
    target instanceof Element ? target.closest(channelSelector) : null;

  document.addEventListener("pointerover", (event) => {
    if (hoveredCard(event.target)) {
      document.documentElement.setAttribute(hoverAttribute, "");
    } else {
      hide();
    }
  }, { passive: true });

  document.addEventListener("pointerout", (event) => {
    const card = hoveredCard(event.target);
    if (card && !(event.relatedTarget instanceof Node && card.contains(event.relatedTarget))) {
      hide();
    }
  }, { passive: true });

  document.addEventListener("pointermove", (event) => {
    if (!hoveredCard(event.target)) hide();
  }, { passive: true });

  document.addEventListener("mouseleave", hide);
  window.addEventListener("blur", hide);
})();
