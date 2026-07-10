import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { I18nProvider } from "./lib/i18n";
import { QueryProvider } from "./lib/queryClient";
import "./design-system/index.css";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryProvider>
      <I18nProvider>
        <App />
      </I18nProvider>
    </QueryProvider>
  </React.StrictMode>,
);
