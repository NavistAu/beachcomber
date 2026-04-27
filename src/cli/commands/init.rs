use std::path::PathBuf;
use std::process::ExitCode;

struct DetectedTool {
    name: &'static str,
    snippet: &'static str,
}

pub fn run_init() -> ExitCode {
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg_config = std::env::var("XDG_CONFIG_HOME").unwrap_or(format!("{home}/.config"));

    let mut detected: Vec<DetectedTool> = Vec::new();

    // Powerlevel10k
    if PathBuf::from(format!("{home}/.p10k.zsh")).exists() {
        detected.push(DetectedTool {
            name: "Powerlevel10k",
            snippet: r#"# Add to your .p10k.zsh — replace native git segment with beachcomber:
# In prompt_git(), replace git status calls with:
#   local branch=$(comb g git.branch .)
#   local dirty=$(comb g git.dirty .)"#,
        });
    }

    // Starship
    if PathBuf::from(format!("{xdg_config}/starship.toml")).exists()
        || std::env::var("STARSHIP_CONFIG").is_ok()
    {
        detected.push(DetectedTool {
            name: "Starship",
            snippet: r#"# Add to starship.toml:
[custom.git_branch]
command = "comb g git.branch ."
when = true
shell = ["sh"]"#,
        });
    }

    // oh-my-tmux
    if PathBuf::from(format!("{home}/.tmux.conf.local")).exists() {
        detected.push(DetectedTool {
            name: "oh-my-tmux",
            snippet: r##"# Add to .tmux.conf.local:
tmux_conf_theme_status_right="#(comb g git.branch .) | #(comb g load.one) | %R""##,
        });
    }

    // tmux (generic)
    if PathBuf::from(format!("{home}/.tmux.conf")).exists() {
        detected.push(DetectedTool {
            name: "tmux",
            snippet: r##"# Add to .tmux.conf:
set -g status-right "#(comb g git.branch .) #(comb g load.one)""##,
        });
    }

    // Neovim
    if PathBuf::from(format!("{xdg_config}/nvim/init.lua")).exists()
        || PathBuf::from(format!("{xdg_config}/nvim/init.vim")).exists()
    {
        detected.push(DetectedTool {
            name: "Neovim",
            snippet: r#"-- Lua statusline integration (lualine, heirline, etc.):
-- local beachcomber = require('libbeachcomber')
-- local client = beachcomber.connect()
-- local branch = client:get_text('git.branch', vim.fn.getcwd())"#,
        });
    }

    // Polybar
    if PathBuf::from(format!("{xdg_config}/polybar/config.ini")).exists()
        || PathBuf::from(format!("{xdg_config}/polybar/config")).exists()
    {
        detected.push(DetectedTool {
            name: "Polybar",
            snippet: r#"# Add to polybar config:
[module/beachcomber-git]
type = custom/script
exec = comb g git.branch .
interval = 2"#,
        });
    }

    // Waybar
    if PathBuf::from(format!("{xdg_config}/waybar/config")).exists()
        || PathBuf::from(format!("{xdg_config}/waybar/config.jsonc")).exists()
    {
        detected.push(DetectedTool {
            name: "Waybar",
            snippet: r#"// Add to waybar config:
"custom/git": {
    "exec": "comb g git.branch .",
    "interval": 2
}"#,
        });
    }

    // Sketchybar
    if PathBuf::from(format!("{xdg_config}/sketchybar/sketchybarrc")).exists()
        || PathBuf::from(format!("{home}/.config/sketchybar/sketchybarrc")).exists()
    {
        detected.push(DetectedTool {
            name: "Sketchybar",
            snippet: r#"# Add to sketchybarrc:
sketchybar --add item git left \
           --set git script="comb g git.branch ." \
           update_freq=2"#,
        });
    }

    // oh-my-zsh
    if PathBuf::from(format!("{home}/.oh-my-zsh")).exists() || std::env::var("ZSH").is_ok() {
        detected.push(DetectedTool {
            name: "Oh My Zsh",
            snippet: r#"# Source the chpwd hook for faster directory switching:
source <(curl -fsSL https://beachcomber.sh/scripts/chpwd.sh)
# Or download and source from a local path."#,
        });
    }

    if detected.is_empty() {
        println!("No supported tools detected.");
        println!();
        println!("beachcomber integrates with: starship, powerlevel10k, oh-my-tmux,");
        println!("tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh, and more.");
        println!();
        println!("See https://beachcomber.sh for integration guides.");
    } else {
        println!(
            "Detected {} tool(s) with beachcomber integration support:",
            detected.len()
        );
        println!();
        for tool in &detected {
            println!("--- {} ---", tool.name);
            println!();
            println!("{}", tool.snippet);
            println!();
        }
        println!("Full integration guides: https://beachcomber.sh");
    }

    ExitCode::SUCCESS
}
