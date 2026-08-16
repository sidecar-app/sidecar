import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "@sidecar/viewer/src/theme.css";
import "./App.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
