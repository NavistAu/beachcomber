import {useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './HowItWorks.module.css';

// Each span: [text, style] where style maps to a CSS module class
type Span = [string, string];
interface TabLine {
  type: string;
  spans?: Span[];
  prompt?: string;
  text?: string;
  annotation?: string;
}
interface Tab {
  id: string;
  label: string;
  lines: TabLine[];
}

const tabs: Tab[] = [
  {
    id: 'cli',
    label: 'CLI',
    lines: [
      {type: 'cmd', prompt: '$ ', text: 'comb get git.branch . -f text'},
      {type: 'out', text: 'main', annotation: '← 15µs'},
      {type: 'spacer'},
      {type: 'cmd', prompt: '$ ', text: 'comb get battery.percent -f text'},
      {type: 'out', text: '85'},
      {type: 'spacer'},
      {type: 'cmd', prompt: '$ ', text: 'comb status'},
      {type: 'out', text: '{"cache_entries": 12, "active_watchers": 3, "demand": 8}'},
    ],
  },
  {
    id: 'python',
    label: 'Python',
    lines: [
      {type: 'spans', spans: [['from ', 'keyword'], ['beachcomber ', 'text'], ['import ', 'keyword'], ['Client', 'type']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['client ', 'text'], ['= ', 'operator'], ['Client', 'function'], ['()', 'text']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['result ', 'text'], ['= ', 'operator'], ['client.', 'text'], ['get', 'function'], ['(', 'text'], ['"git.branch"', 'string'], [', path=', 'text'], ['"."', 'string'], [')', 'text']]},
      {type: 'spans', spans: [['if ', 'keyword'], ['result.is_hit:', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['print', 'function'], ['(', 'text'], ['f"Branch: {result.data}"', 'string'], [')', 'text']]},
      {type: 'spacer'},
      {type: 'comment', text: '# Persistent session for multiple queries'},
      {type: 'spans', spans: [['with ', 'keyword'], ['client.', 'text'], ['session', 'function'], ['() ', 'text'], ['as ', 'keyword'], ['s:', 'text']]},
      {type: 'spans', spans: [['    s.', 'text'], ['set_context', 'function'], ['(', 'text'], ['"."', 'string'], [')', 'text']]},
      {type: 'spans', spans: [['    branch ', 'text'], ['= ', 'operator'], ['s.', 'text'], ['get', 'function'], ['(', 'text'], ['"git.branch"', 'string'], [')', 'text']]},
      {type: 'spans', spans: [['    battery ', 'text'], ['= ', 'operator'], ['s.', 'text'], ['get', 'function'], ['(', 'text'], ['"battery.percent"', 'string'], [')', 'text']]},
    ],
  },
  {
    id: 'rust',
    label: 'Rust',
    lines: [
      {type: 'spans', spans: [['use ', 'keyword'], ['beachcomber_client', 'text'], ['::{', 'text'], ['Client', 'type'], [', ', 'text'], ['CombResult', 'type'], ['};', 'text']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['let ', 'keyword'], ['client ', 'text'], ['= ', 'operator'], ['Client', 'type'], ['::', 'text'], ['new', 'function'], ['();', 'text']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['match ', 'keyword'], ['client.', 'text'], ['get', 'function'], ['(', 'text'], ['"git.branch"', 'string'], [', ', 'text'], ['Some', 'function'], ['(', 'text'], ['"."', 'string'], ['))? {', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['CombResult', 'type'], ['::', 'text'], ['Hit', 'function'], [' { data, .. } ', 'text'], ['=> ', 'operator'], ['{', 'text']]},
      {type: 'spans', spans: [['        ', 'text'], ['println!', 'function'], ['(', 'text'], ['"branch: {}"', 'string'], [', data.', 'text'], ['as_text', 'function'], ['().', 'text'], ['unwrap', 'function'], ['());', 'text']]},
      {type: 'spans', spans: [['    }', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['CombResult', 'type'], ['::', 'text'], ['Miss', 'function'], [' => ', 'operator'], ['{}', 'text']]},
      {type: 'spans', spans: [['}', 'text']]},
    ],
  },
  {
    id: 'lua',
    label: 'Lua / Neovim',
    lines: [
      {type: 'comment', text: '-- neovim statusline using vim.uv'},
      {type: 'spans', spans: [['local ', 'keyword'], ['comb ', 'text'], ['= ', 'operator'], ['require', 'function'], ['(', 'text'], ['"beachcomber"', 'string'], [')', 'text']]},
      {type: 'spans', spans: [['local ', 'keyword'], ['client ', 'text'], ['= ', 'operator'], ['comb.', 'text'], ['connect', 'function'], ['()', 'text']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['local ', 'keyword'], ['function ', 'keyword'], ['git_branch', 'function'], ['()', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['local ', 'keyword'], ['cwd ', 'text'], ['= ', 'operator'], ['vim.fn.', 'text'], ['getcwd', 'function'], ['()', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['local ', 'keyword'], ['result ', 'text'], ['= ', 'operator'], ['client:', 'text'], ['get', 'function'], ['(', 'text'], ['"git.branch"', 'string'], [', cwd)', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['if ', 'keyword'], ['result ', 'text'], ['and ', 'keyword'], ['result:', 'text'], ['is_hit', 'function'], ['() ', 'text'], ['then', 'keyword']]},
      {type: 'spans', spans: [['        ', 'text'], ['return ', 'keyword'], ['" "', 'string'], [' .. ', 'operator'], ['result.data', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['end', 'keyword']]},
      {type: 'spans', spans: [['    ', 'text'], ['return ', 'keyword'], ['""', 'string']]},
      {type: 'spans', spans: [['end', 'keyword']]},
    ],
  },
  {
    id: 'zsh',
    label: 'zsh',
    lines: [
      {type: 'comment', text: '# ~/.zshrc'},
      {type: 'spans', spans: [['precmd', 'function'], ['() {', 'text']]},
      {type: 'spans', spans: [['    ', 'text'], ['local ', 'keyword'], ['branch dirty', 'text']]},
      {type: 'spans', spans: [['    branch=', 'text'], ['$(', 'operator'], ['comb get git.branch . -f text', 'text'], [' 2>/dev/null)', 'operator']]},
      {type: 'spans', spans: [['    dirty=', 'text'], ['$(', 'operator'], ['comb get git.dirty . -f text', 'text'], [' 2>/dev/null)', 'operator']]},
      {type: 'spacer'},
      {type: 'spans', spans: [['    ', 'text'], ['local ', 'keyword'], ['git_part=', 'text'], ['""', 'string']]},
      {type: 'spans', spans: [['    ', 'text'], ['if ', 'keyword'], ['[[ -n ', 'text'], ['"$branch"', 'string'], [' ]]; ', 'text'], ['then', 'keyword']]},
      {type: 'spans', spans: [['        git_part=', 'text'], ['"%F{blue}${branch}%f"', 'string']]},
      {type: 'spans', spans: [['        [[ ', 'text'], ['"$dirty"', 'string'], [' == ', 'operator'], ['"true"', 'string'], [' ]] && git_part+=', 'text'], ['"*"', 'string']]},
      {type: 'spans', spans: [['    ', 'text'], ['fi', 'keyword']]},
      {type: 'spans', spans: [['    PS1=', 'text'], ['"${git_part} %# "', 'string']]},
      {type: 'spans', spans: [['}', 'text']]},
    ],
  },
  {
    id: 'tmux',
    label: 'tmux',
    lines: [
      {type: 'comment', text: '# ~/.tmux.conf'},
      {type: 'spacer'},
      {type: 'comment', text: '# Battery + git branch in status bar'},
      {type: 'spans', spans: [['set ', 'keyword'], ['-g status-right ', 'text'], ['\\', 'operator']]},
      {type: 'spans', spans: [['  ', 'text'], ["'#(comb get battery.percent -f text)%% |", 'string'], [' \\', 'operator']]},
      {type: 'spans', spans: [['    ', 'text'], ["#(comb get git.branch . -f text)'", 'string']]},
      {type: 'spacer'},
      {type: 'comment', text: '# Kubernetes context in left status'},
      {type: 'spans', spans: [['set ', 'keyword'], ['-g status-left ', 'text'], ['\\', 'operator']]},
      {type: 'spans', spans: [['  ', 'text'], ["'[#S] #(comb get kubecontext.context -f text)'", 'string']]},
      {type: 'spacer'},
      {type: 'comment', text: '# Low interval is fine — queries cost ~34µs'},
      {type: 'spans', spans: [['set ', 'keyword'], ['-g status-interval ', 'text'], ['5', 'number']]},
    ],
  },
];

const styleMap: Record<string, string> = {
  text: styles.codeText,
  keyword: styles.codeKeyword,
  string: styles.codeString,
  function: styles.codeFunction,
  type: styles.codeType,
  number: styles.codeNumber,
  operator: styles.codeOperator,
  comment: styles.codeComment,
};

function TerminalLine({line}: {line: TabLine}) {
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
    case 'spans':
      return (
        <div className={styles.codeLine}>
          {line.spans?.map(([text, style], i) => (
            <span key={i} className={styleMap[style] || styles.codeText}>{text}</span>
          ))}
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
            <div className={styles.dots}>
              <span className={`${styles.dot} ${styles.dotRed}`} />
              <span className={`${styles.dot} ${styles.dotYellow}`} />
              <span className={`${styles.dot} ${styles.dotGreen}`} />
            </div>
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
