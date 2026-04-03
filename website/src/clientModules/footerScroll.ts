import ExecutionEnvironment from '@docusaurus/ExecutionEnvironment';

if (ExecutionEnvironment.canUseDOM) {
  const fadeStart = 100;
  const fadeEnd = 500;

  function isLandingPage(): boolean {
    return window.location.pathname === '/' || window.location.pathname === '';
  }

  function updateFooter() {
    const footer = document.querySelector('.footer') as HTMLElement;
    if (!footer) return;

    if (isLandingPage()) {
      const opacity = Math.min(1, Math.max(0, (window.scrollY - fadeStart) / (fadeEnd - fadeStart)));
      footer.style.opacity = String(opacity);
    } else {
      footer.style.opacity = '1';
    }
  }

  window.addEventListener('scroll', updateFooter, {passive: true});
  window.addEventListener('popstate', updateFooter);
  // Observe DOM changes to catch Docusaurus client-side navigation
  new MutationObserver(updateFooter).observe(document.body, {childList: true, subtree: true});
  updateFooter();
}
