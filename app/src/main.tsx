import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { TipProvider } from "./components/ui";
import "./index.css";
import { SettingsProvider } from "./lib/settings";
import { StatusProvider } from "./lib/status";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <SettingsProvider>
      <TipProvider>
        <StatusProvider>
          <App />
        </StatusProvider>
      </TipProvider>
    </SettingsProvider>
  </StrictMode>,
);
