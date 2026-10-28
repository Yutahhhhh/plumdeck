import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./components/analysis-layout.css";
import { ThemeProvider } from "./components/theme-provider";

class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { hasError: boolean; error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error) {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("Uncaught error:", error, errorInfo);
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="p-4 text-red-500">
          <h1>Something went wrong.</h1>
          <pre>{this.state.error?.message}</pre>
          <pre>{this.state.error?.stack}</pre>
        </div>
      );
    }

    return this.props.children;
  }
}

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

// Dev-only: ?preview=startup renders the startup screen with fixture data in a
// plain browser (no Tauri/backend). Vite folds import.meta.env.DEV to false in
// production, so this branch and its chunk are dropped from release builds.
if (import.meta.env.DEV && new URLSearchParams(window.location.search).get("preview") === "startup") {
  void import("./components/StartupPreview").then(({ StartupPreview }) => {
    root.render(
      <React.StrictMode>
        <StartupPreview />
      </React.StrictMode>,
    );
  });
} else {
  root.render(
    <React.StrictMode>
      <ErrorBoundary>
        <ThemeProvider defaultTheme="system" storageKey="plumdeck-ui-theme">
          <App />
        </ThemeProvider>
      </ErrorBoundary>
    </React.StrictMode>,
  );
}
