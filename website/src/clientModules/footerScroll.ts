import ExecutionEnvironment from '@docusaurus/ExecutionEnvironment';

if (ExecutionEnvironment.canUseDOM) {
  const fadeStart = 100;
  const fadeEnd = 500;

  function isLandingPage(): boolean {
    return window.location.pathname === '/' || window.location.pathname === '';
  }

  // The home page's hero carries the name, so the navbar's brand (and, on a small screen, the whole navbar)
  // waits until the hero has scrolled away.
  function updateBrand() {
    const hero = isLandingPage() ? document.getElementById('hero') : null;
    // the page starts with the hero in view (custom.css keys on #hero), so only its leaving is marked: once
    // no more of it shows than the navbar would cover
    const bar = document.querySelector<HTMLElement>('.navbar')?.offsetHeight ?? 0;
    document.documentElement.classList.toggle('hero-gone', !!hero && hero.getBoundingClientRect().bottom <= bar + 1);
  }

  function updateFooter() {
    updateBrand();
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
