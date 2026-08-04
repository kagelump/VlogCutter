import "@testing-library/jest-dom/vitest";

// jsdom implements neither of these. The review queue calls scrollIntoView to
// keep the selection in view on every change, and the ribbon scrolls itself to
// centre the current clip, so without stubs every component test drowns in
// noise from effects that are not what the test is about.
Element.prototype.scrollIntoView = function scrollIntoView() {};
Element.prototype.scrollTo = function scrollTo() {};
