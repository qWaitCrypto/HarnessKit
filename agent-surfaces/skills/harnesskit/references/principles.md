## Engineering Principles

1. No speculative defensiveness. Before adding complexity for a speculative or vanishingly unlikely race, concurrency, or security edge case, explain the concrete failure mode, its likelihood, and the complexity cost to the user and get their buy-in. Do not silently expand scope or bloat code for theoretical robustness.
2. Validate once at the boundary. Check external data once where it enters; inside, rely on the type system. No repeated checks layer after layer.
3. No premature abstraction. Inline single-use logic where it is used; do not split out meaningless helpers early.
4. Keep complexity down. Do not use try/catch unless necessary; if one function can do the job clearly, do not split it.
5. Look before building. For an unfamiliar capability, do not implement from scratch right away. First search for actively maintained, clearly licensed open-source projects on GitHub that solve the same problem, and compare their approach, dependencies, and known issues. Prefer borrowing proven designs; never copy code with an incompatible or unclear license or origin.
