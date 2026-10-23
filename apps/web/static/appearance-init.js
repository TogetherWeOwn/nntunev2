/* Pre-paint appearance init (render-blocking same-origin script; strict CSP unchanged). Applies a saved light/dark override before first paint; reads only the validated enum, never writes. */
(function(){try{var v=localStorage.getItem('nntune:appearance:v1');if(v==='light'||v==='dark')document.documentElement.setAttribute('data-appearance',v);}catch{}})();
