import styles from './Integrations.module.css';

const integrations = [
  'zsh',
  'bash',
  'fish',
  'tmux',
  'neovim',
  'starship',
  'polybar',
  'waybar',
  'sketchybar',
];

export default function Integrations(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>Works with everything</h2>
        <p className={styles.subheading}>beachcomber is infrastructure, not a prompt theme</p>
        <div className={styles.items}>
          {integrations.map((name) => (
            <div key={name} className={styles.item}>
              {name}
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
