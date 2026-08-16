import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { OverlayWindow } from "./OverlayWindow";
import { StatsWindow } from "./StatsWindow";
import "@sidecar/viewer/src/theme.css";
import "./index.css";

const rootElement = document.getElementById("root") as HTMLElement;
const root = ReactDOM.createRoot(rootElement);

// Route based on window query parameter
const params = new URLSearchParams(window.location.search);
const windowType = params.get("window");

root.render(
  <React.StrictMode>
    {windowType === "overlay"
      ? <OverlayWindow />
      : windowType === "stats"
      ? <StatsWindow />
      : <App />}
  </React.StrictMode>,
);
