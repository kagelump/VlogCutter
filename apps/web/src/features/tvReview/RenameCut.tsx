import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { ReviewTargetSummary } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { Button, Dialog, ErrorMessage, Field } from "../../ui";

export function RenameCut({ projectId, target }: { projectId: string; target: ReviewTargetSummary }) {
  const client = useClient();
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const save = useMutation({
    mutationFn: () => client.renameReviewTarget(projectId, target.reviewTargetId, name.trim() || null),
    onSuccess: (detail) => {
      queryClient.setQueryData(keys.target(projectId, target.reviewTargetId), detail);
      void queryClient.invalidateQueries({ queryKey: keys.targets(projectId) });
      setOpen(false);
    },
  });
  return <>
    <Button onClick={() => { setName(target.displayName ?? ""); save.reset(); setOpen(true); }}>Rename cut</Button>
    <Dialog open={open} onClose={() => { if (!save.isPending) setOpen(false); }} title="Rename cut" footer={<>
      <Button disabled={save.isPending} onClick={() => setOpen(false)}>Cancel</Button>
      <Button variant="primary" busy={save.isPending} onClick={() => save.mutate()}>Save name</Button>
    </>}>
      <Field label="Cut name" hint="For example, RC7. Leave blank to use the revision ID.">
        {(id) => <input id={id} value={name} maxLength={120} onChange={(event) => setName(event.target.value)} autoFocus />}
      </Field>
      <ErrorMessage error={save.error} />
    </Dialog>
  </>;
}
