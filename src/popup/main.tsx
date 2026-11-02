import React from "react";
import ReactDOM from "react-dom/client";
import Popup from "./Popup";
import "../styles/tokens.css";

ReactDOM.createRoot(document.getElementById("popup-root") as HTMLElement).render(
  <React.StrictMode>
    <Popup />
  </React.StrictMode>,
);
