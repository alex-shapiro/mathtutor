# Math Tutor

Overall design doc: docs/design.md

## Playbooks

- Follow playbooks/feature.md for a feature request.
- Follow playbooks/review.md for a system or code review request.
- Follow playbooks/copyedit.md for a copy editing request.

If playbook instructions conflict with this doc, follow the playbook and communicate the conflict to the user.

## Be Bold

Approach each task from first principles. This is a greenfield codebase with no legacy code to maintain. You own the whole codebase and can make any change, anywhere.

## Symbol Names

Symbol names must be idiomatic and just specific enough that a reader can

- understand it without requiring surrounding context
- distinguish it from other symbols with similar names but different uses
- connect it to its related symbols (example: a getter and its return type are related)

| bad                  | good                      |
| -------------------- | ------------------------- |
| ReadId               | StdinReadId               |
| Who                  | DocOwner                  |
| wanted               | has_waiting_readers       |
| to                   | target                    |
| until(...) -> Future | wait_until(...) -> Future |
| wait() -> WaitKind   | wait_kind() -> WaitKind   |

## Comment Guidelines

A symbol's doc comment, and especially its first line, must reflect the symbol's purpose. For example, if a type represents an HTTP route, the type's doc comment should be its HTTP route because its purpose is to serve that route. Avoid using a doc comment to elaborate on implementation details that the caller does not care about. If such implementation details are actually necessary because of non-obvious code, flag it to the user.

Before writing a code comment, ask yourself whether the comment is reflective of poorly written code. Is it an expression of code that is so intrinsically complex that the extra writing is the only way to make the code understandable? Typically, there is a better way to make the code or interface understandable. Exceptions to this rule tend to be interfaces that _do_ have inherent complexity, like a regex library or policy file format.

Avoid changelog-style comments at all cost. They pollute the codebase and harm the reader's understanding of the code as it exists.

## Writing and User Communication Guidelines

Every output that is not code, including docs, comments, and communication with the user, must be written in AST-STE100 English. Convey key ideas in leading sentences to ensure they are easy to find. Avoid runs of facts and symbols, which are impossible for a human to parse. If you generate a run of facts and symbols, step back and consider how to apply AST-STE100 guidelines to structure output more effectively. See the copy editing playbook for detailed guidelines.

Remove all redundancies and overly verbose phrases you find while reading.

Never write with middle voice. If you read something that already contains middle voice, rewrite it to follow AST-STE100 guidelines.

Communication Anti-patterns that Claude, in particular, must avoid:

- Writing carefully, for self-protection, instead of clearly. This leads to legalese, jargon, hedges, and qualifiers.
- Swinging between too little context and unnecessary padding in answers.
- Failing to provide context when the user needs to make a decision.
- Adding questions and irrelevant options when the user asks a simple question.
- Fixating on a narrow interpretation and ignoring a larger truth.
