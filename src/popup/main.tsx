import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/inter/opsz.css";
import "../styles/tokens.css";
import "./popup.css";
import Popup from "./Popup";

ReactDOM.createRoot(document.getElementById("popup-root") as HTMLElement).render(
  <React.StrictMode>
    <Popup />
  </React.StrictMode>,
);
