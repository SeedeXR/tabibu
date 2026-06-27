// Shared Mermaid theme + init for the docs site — used by both the landing page
// (index.html) and every generated doc page, so the diagram styling can't drift.
// Load after mermaid.min.js.
mermaid.initialize({
  startOnLoad: true,
  theme: "base",
  themeVariables: {
    fontFamily: "Space Grotesk, sans-serif",
    background: "#0a1d27",
    primaryColor: "#0e2531", primaryBorderColor: "#2f8ed4", primaryTextColor: "#eaf3f6",
    lineColor: "#5d727c", secondaryColor: "#112c39", tertiaryColor: "#04323f",
    noteBkgColor: "#13313f", noteTextColor: "#eaf3f6", noteBorderColor: "#ec6a40",
    actorBkg: "#0e2531", actorBorder: "#2f8ed4", actorTextColor: "#eaf3f6",
    signalColor: "#8ba1ac", signalTextColor: "#cfe0e6",
    labelBoxBkgColor: "#112c39", labelTextColor: "#eaf3f6",
  },
});
