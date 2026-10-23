// swagger-ui-dist ships no types; we use only the bundle factory + its CSS.
declare module 'swagger-ui-dist/swagger-ui-bundle.js' {
  const SwaggerUIBundle: (opts: Record<string, unknown>) => unknown;
  export default SwaggerUIBundle;
}
declare module 'swagger-ui-dist/swagger-ui.css';
