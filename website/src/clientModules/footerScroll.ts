import ExecutionEnvironment from '@docusaurus/ExecutionEnvironment';

if (ExecutionEnvironment.canUseDOM) {
  const threshold = 300;

  function updateFooter() {
    const footer = document.querySelector('.footer');
    if (!footer) return;

    if (window.scrollY > threshold) {
      footer.classList.add('footer--visible');
    } else {
      footer.classList.remove('footer--visible');
    }
  }

  window.addEventListener('scroll', updateFooter, {passive: true});
  // Initial check
  updateFooter();
}
