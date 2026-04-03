import {useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './HowItWorks.module.css';

const tabs = [
  {
    id: 'cli',
    label: 'CLI',
    lines: [
      {type: 'cmd', prompt: '$', text: ' comb get git.branch . -f text'},
      {type: 'out', text: 'main', annotation: '← 15µs'},
      {type: 'spacer'},
      {type: 'cmd', prompt: '$', text: ' comb get battery.percent -f text'},
      {type: 'out', text: '85'},
      {type: 'spacer'},
      {type: 'cmd', prompt: '$', text: ' comb status'},
      {type: 'json', text: '{"cache_entries": 12, "active_watchers": 3, "demand": 8}'},
    ],
  },
  {
    id: 'python',
    label: 'Python',
    lines: [
      {type: 'code', text: 'from beachcomber import Client', color: 'keyword'},
      {type: 'spacer'},
      {type: 'code', text: 'client = Client()'},
      {type: 'spacer'},
      {type: 'code', text: 'result = client.get("git.branch", path=".")'},
      {type: 'code', text: 'if result.is_hit:'},
      {type: 'code', text: '    print(f"Branch: {result.data}")'},
      {type: 'spacer'},
      {type: 'comment', text: '# Persistent session for multiple queries'},
      {type: 'code', text: 'with client.session() as s:'},
      {type: 'code', text: '    s.set_context(".")'},
      {type: 'code', text: '    branch = s.get("git.branch")'},
      {type: 'code', text: '    battery = s.get("battery.percent")'},
    ],
  },
  {
    id: 'rust',
    label: 'Rust',
    lines: [
      {type: 'code', text: 'use beachcomber_client::{Client, CombResult};', color: 'keyword'},
      {type: 'spacer'},
      {type: 'code', text: 'let client = Client::new();'},
      {type: 'spacer'},
      {type: 'code', text: 'match client.get("git.branch", Some("."))? {'},
      {type: 'code', text: '    CombResult::Hit { data, .. } => {'},
      {type: 'code', text: '        println!("branch: {}", data.as_text().unwrap());'},
      {type: 'code', text: '    }'},
      {type: 'code', text: '    CombResult::Miss => {}'},
      {type: 'code', text: '}'},
    ],
  },
  {
    id: 'lua',
    label: 'Lua / Neovim',
    lines: [
      {type: 'comment', text: '-- neovim statusline using vim.uv'},
      {type: 'code', text: 'local comb = require("beachcomber")', color: 'keyword'},
      {type: 'code', text: 'local client = comb.connect()'},
      {type: 'spacer'},
      {type: 'code', text: 'local function git_branch()'},
      {type: 'code', text: '    local cwd = vim.fn.getcwd()'},
      {type: 'code', text: '    local result = client:get("git.branch", cwd)'},
      {type: 'code', text: '    if result and result:is_hit() then'},
      {type: 'code', text: '        return " " .. result.data'},
      {type: 'code', text: '    end'},
      {type: 'code', text: '    return ""'},
      {type: 'code', text: 'end'},
    ],
  },
  {
    id: 'zsh',
    label: 'zsh',
    lines: [
      {type: 'comment', text: '# ~/.zshrc'},
      {type: 'code', text: 'precmd() {', color: 'keyword'},
      {type: 'code', text: '    local branch dirty'},
      {type: 'code', text: '    branch=$(comb get git.branch . -f text 2>/dev/null)'},
      {type: 'code', text: '    dirty=$(comb get git.dirty . -f text 2>/dev/null)'},
      {type: 'spacer'},
      {type: 'code', text: '    local git_part=""'},
      {type: 'code', text: '    if [[ -n "$branch" ]]; then'},
      {type: 'code', text: '        git_part="%F{blue}${branch}%f"'},
      {type: 'code', text: '        [[ "$dirty" == "true" ]] && git_part+="*"'},
      {type: 'code', text: '    fi'},
      {type: 'code', text: '    PS1="${git_part} %# "'},
      {type: 'code', text: '}'},
    ],
  },
  {
    id: 'tmux',
    label: 'tmux',
    lines: [
      {type: 'comment', text: '# ~/.tmux.conf'},
      {type: 'spacer'},
      {type: 'comment', text: '# Battery + git branch in status bar'},
      {type: 'code', text: 'set -g status-right \\', color: 'keyword'},
      {type: 'code', text: '  \'#(comb get battery.percent -f text)%% | \\'},
      {type: 'code', text: '    #(comb get git.branch . -f text)\''},
      {type: 'spacer'},
      {type: 'comment', text: '# Kubernetes context in left status'},
      {type: 'code', text: 'set -g status-left \\', color: 'keyword'},
      {type: 'code', text: '  \'[#S] #(comb get kubecontext.context -f text)\''},
      {type: 'spacer'},
      {type: 'comment', text: '# Low interval is fine — queries cost ~34µs'},
      {type: 'code', text: 'set -g status-interval 5', color: 'keyword'},
    ],
  },
];

interface Line {
  type: string;
  prompt?: string;
  text?: string;
  annotation?: string;
  color?: string;
}

function TerminalLine({line}: {line: Line}) {
  switch (line.type) {
    case 'cmd':
      return (
        <div className={styles.terminalLine}>
          <span className={styles.prompt}>{line.prompt}</span>
          <span className={styles.command}>{line.text}</span>
        </div>
      );
    case 'out':
      return (
        <div className={styles.terminalLine}>
          <span className={styles.output}>{line.text}</span>
          {line.annotation && <span className={styles.annotation}>{line.annotation}</span>}
        </div>
      );
    case 'json':
      return (
        <div className={styles.terminalLine}>
          <span className={styles.output}>{line.text}</span>
        </div>
      );
    case 'code':
      return (
        <div className={styles.codeLine}>
          <span className={line.color === 'keyword' ? styles.codeKeyword : styles.codeText}>
            {line.text}
          </span>
        </div>
      );
    case 'comment':
      return (
        <div className={styles.codeLine}>
          <span className={styles.codeComment}>{line.text}</span>
        </div>
      );
    case 'spacer':
      return <div className={styles.terminalSpacer} />;
    default:
      return null;
  }
}

export default function HowItWorks(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);
  const [activeTab, setActiveTab] = useState('cli');

  const activeContent = tabs.find(t => t.id === activeTab);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <div className={styles.label}>How it works</div>
        <h2 className={styles.heading}>How it works</h2>
        <p className={styles.description}>
          beachcomber is a single async daemon. It watches directories using native OS APIs, runs
          providers when files change or timers fire, and caches results in a shared in-memory map.
          Every consumer reads from the same cache via a Unix socket.
        </p>

        <div className={styles.terminalWrapper}>
          <div className={styles.terminalHeader}>
            <span className={`${styles.dot} ${styles.dotRed}`} />
            <span className={`${styles.dot} ${styles.dotYellow}`} />
            <span className={`${styles.dot} ${styles.dotGreen}`} />
            <div className={styles.tabs}>
              {tabs.map(tab => (
                <button
                  key={tab.id}
                  className={`${styles.tab} ${activeTab === tab.id ? styles.tabActive : ''}`}
                  onClick={() => setActiveTab(tab.id)}
                >
                  {tab.label}
                </button>
              ))}
            </div>
          </div>
          <div className={styles.terminalBody}>
            {activeContent?.lines.map((line, i) => (
              <TerminalLine key={`${activeTab}-${i}`} line={line} />
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
