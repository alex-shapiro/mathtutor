# Tradeoffs

Open product decisions from the branch review. Each section states the problem and the options.

## A lost refresh response signs the connector out

A refresh request to `/oauth/token` deletes the presented refresh token in the transaction that issues the new pair. If the response does not reach the connector, the connector holds only the deleted token, and its next refresh fails. The user must sign in again.

- **Keep the old token valid for a short grace window:** A lost response has zero effect. A stolen refresh token stays usable for longer.
- **Return the same new pair if the old token comes back within the window:** A lost response has zero effect, and the server issues no extra secret. A stolen refresh token has the same longer exposure.
- **Keep strict rotation:** A stolen token works only once. A lost response signs the connector out.

## Who creates the database

The first `mt path new` creates the database. Only a key without a database limit can create a database, and such a key can also delete every database of its user. Thus the first session of an agent must hold a key that can delete all learner history.

- **Add an operator setup command, such as `mt db create`:** The operator runs it once with a temporary unlimited key. After that, every learner command uses a key that is limited to the database. A new user has an extra setup step.
- **Keep creation in `mt path new`:** Setup has no extra step. The agent holds an unlimited key on first use.

## Sign-in delay after incorrect passwords

`/oauth/authorize` delays the next attempt from an address after 5 incorrect passwords. The wait doubles from 1 second up to a limit of 15 minutes, so an attacker at the limit can try about 4 passwords per hour from each address. The limit also applies to the administrator on the same address, and to every user behind the same NAT or `/64` IPv6 network. The server uses 15 minutes now.

- **Shorter limit, such as 1 minute:** A user who mistypes many times waits less. An attacker can try about 60 passwords per hour from each address.
- **Longer limit, such as 1 hour:** An attacker can try 1 password per hour from each address. A user who mistypes many times can wait up to 1 hour, or must restart the server.
- **Add a global limit on failures from all addresses:** An attacker with many addresses gets few attempts. An attacker can lock out the administrator from every address.
