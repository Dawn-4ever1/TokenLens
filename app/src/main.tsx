import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { TipProvider } from "./components/ui";
import "./index.css";
import { SettingsProvider } from "./lib/settings";
import { StatusProvider } from "./lib/status";
import { UpdateProvider } from "./lib/updates";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <SettingsProvider>
      <TipProvider>
        <StatusProvider>
          <UpdateProvider>
            <App />
          </UpdateProvider>
        </StatusProvider>
      </TipProvider>
    </SettingsProvider>
  </StrictMode>,
);
