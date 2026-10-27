# Working agreements

- The root agent orchestrates architecture, decisions and coordination. Delegate implementation, research, troubleshooting and verification to subagents, as requested by the user.
- Make one atomic commit for each completed, coherent change. Do not create commits for read-only commands or intermediate unfinished steps.
- At the end of authorized work, push the completed commits to the authorized GitHub origin without force, then verify local and remote HEAD agree.
- Stage explicit files belonging to the change. Do not commit secrets, Minecraft worlds, JARs, runtime data or build outputs.
- Run checks appropriate to the change and report material limitations or external blockers honestly.
