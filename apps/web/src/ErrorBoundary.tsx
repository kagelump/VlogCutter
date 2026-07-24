import { Component, type ErrorInfo, type ReactNode } from "react";

export class AppErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, details: ErrorInfo) {
    console.error("web_shell_render_failed", error, details);
  }

  render() {
    if (this.state.failed) {
      return (
        <main className="fatal-state">
          <p className="eyebrow">Application error</p>
          <h1>The review desk could not open.</h1>
          <p>Reload the page. If the problem continues, record the time and check the server logs.</p>
          <button onClick={() => window.location.reload()}>Reload</button>
        </main>
      );
    }
    return this.props.children;
  }
}
