import { installMockServer } from "./mockServer";

// Must run before any API calls (i.e. before React renders)
const params = new URLSearchParams(window.location.search);
if (params.get("mock") !== "false") {
  installMockServer();
}

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../../ui/src/theme.css";
import App from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
