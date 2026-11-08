import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { useDocumentTitle } from "../../lib/hooks";
import { Button, ErrorMessage, Field } from "../../ui";
import styles from "./LoginPage.module.css";

export function LoginPage() {
  const client = useClient();
  const queryClient = useQueryClient();
  const [code, setCode] = useState("");
  useDocumentTitle("Sign in");

  const login = useMutation({
    mutationFn: (value: string) => client.loginWithBootstrapCode(value),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: keys.auth }),
  });

  function submit(event: FormEvent) {
    event.preventDefault();
    const value = code.trim();
    if (value) login.mutate(value);
  }

  return (
    <main className={styles.layout}>
      <form className={styles.card} onSubmit={submit}>
        <div className={styles.brand}>VlogCutter</div>
        <h1>Sign in</h1>
        <p className={styles.lede}>Enter a one-time sign-in code from the server.</p>
        <Field label="Sign-in code" hint={<>Create one on the server with <code>rcdvlog-server admin bootstrap create</code>. Each code works once.</>}>
          {(id) => (
            <input
              id={id}
              value={code}
              onChange={(event) => setCode(event.target.value)}
              autoComplete="one-time-code"
              autoCapitalize="off"
              spellCheck={false}
              autoFocus
              disabled={login.isPending}
            />
          )}
        </Field>
        {login.error ? <ErrorMessage error={login.error} /> : null}
        <Button type="submit" variant="primary" size="lg" block busy={login.isPending} disabled={!code.trim()}>Sign in</Button>
      </form>
    </main>
  );
}
