# Bottom Control Hover Tips

Draft tooltip text for the bottom control bar. This document is for review only; no tooltip implementation has been added yet.

| Control | Proposed hover text | Notes |
|---|---|---|
| New Game | Start a fresh game from the standard board setup. | Hidden while viewing a replay. In Play mode, this returns to the singleplayer setup screen. |
| Start Game | Start a game from the board currently configured in the editor. | Only shown in Editor mode. |
| Replay: First | Jump to the initial position in this replay. | Only shown in Replay board mode. |
| Replay: Previous | Step back one position in this replay. | Only shown in Replay board mode. |
| Replay position slider | Drag to jump to a specific position in this replay. | Only shown in Replay board mode. |
| Replay: Next | Step forward one position in this replay. | Only shown in Replay board mode. |
| Replay: Last | Jump to the final position in this replay. | Only shown in Replay board mode. |
| Bot Move | Ask the bot to choose and play a move using the current search budget. | Hidden in singleplayer and replay modes. Disabled when replaying. |
| Search | Run analysis from the current position using the selected budget. | Uses either Sims or Depth, depending on the selected budget mode. |
| Pause | Stop the running search and keep the current analysis results. | Active while search is running. |
| Sims | Use a simulation-count budget for search. | The number input controls how many MCTS simulations to run. |
| Depth | Use a principal-variation depth target for search. | The number input controls the target PV depth; simulations may still cap the search when configured by the server. |
| Budget value | Set the search budget value for the selected mode. | Label changes between `Sims` and `Depth`. |
| Options | Open search and move automation options. | Hidden when options are unavailable or in replay mode. |
| Apply | After a manual search finishes, automatically play the best move from that search. | Option in the Options menu. |
| Auto-play | Keep searching and playing moves automatically until the game stops or this is turned off. | Option in the Options menu. Not available in replay mode. |
| Auto-search | Automatically keep analysis search running on each new position. | Option in the Options menu. Not available in replay mode. |

