import ExecutionEnvironment from '@docusaurus/ExecutionEnvironment';

if (ExecutionEnvironment.canUseDOM) {
  const fadeStart = 100;
  const fadeEnd = 500;

  function updateFooter() {
    const footer = document.querySelector('.footer') as HTMLElement;
    if (!footer) return;

    const opacity = Math.min(1, Math.max(0, (window.scrollY - fadeStart) / (fadeEnd - fadeStart)));
    footer.style.opacity = String(opacity);
  }

  window.addEventListener('scroll', updateFooter, {passive: true});
  updateFooter();
}
