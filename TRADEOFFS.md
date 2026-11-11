# Tradeoffs

Open product decisions from the branch review. Each section states the problem and the options.

## A lost refresh response signs the connector out

A refresh request to `/oauth/token` deletes the presented refresh token in the transaction that issues the new pair. If the response does not reach the connector, the connector holds only the deleted token, and its next refresh fails. The user must sign in again.

- **Keep the old token valid for a short grace window:** A lost response has zero effect. A stolen refresh token stays usable for longer.
- **Return the same new pair if the old token comes back within the window:** A lost response has zero effect, and the server issues no extra secret. A stolen refresh token has the same longer exposure.
- **Keep strict rotation:** A stolen token works only once. A lost response signs the connector out.
