import { useDeferredValue, useMemo, useState } from "react";
import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";
import type { ProjectAssetItem, ReviewTargetCut } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { paths } from "../../app/paths";
import { formatClock } from "../../lib/format";
import { Icon } from "../../ui";
import styles from "./ShotGrid.module.css";

const PAGE = 200;
const DESCRIPTION_LABELS = 4;

/** Every clip in the project, keyed by asset ID. Shots use it for their descriptions. */
function useAssetIndex(projectId: string) {
  const client = useClient();
  return useQuery({
    queryKey: [...keys.project(projectId), "asset-index"],
    staleTime: 5 * 60_000,
    queryFn: async () => {
      const index = new Map<string, ProjectAssetItem>();
      let after: string | undefined;
      do {
        const page = await client.listProjectAssets(projectId, PAGE, after);
        for (const item of page.items) index.set(item.projectAssetId, item);
        after = page.nextCursor ?? undefined;
      } while (after);
      return index;
    },
  });
}

/** The clip's analysis description when it has one, else its labels, else its filename. */
function describe(asset: ProjectAssetItem | undefined) {
  if (!asset) return "";
  if (asset.description) return asset.description;
  return asset.labels.length ? asset.labels.slice(0, DESCRIPTION_LABELS).join(", ") : asset.filename;
}

function formatLength(seconds: number) {
  return seconds < 60 ? `${seconds.toFixed(1)}s` : formatClock(seconds);
}

export function ShotGrid({ projectId, cuts }: { projectId: string; cuts: ReviewTargetCut[] }) {
  const client = useClient();
  const assets = useAssetIndex(projectId);
  const [filter, setFilter] = useState("");
  const query = useDeferredValue(filter.trim().toLowerCase());

  const shots = useMemo(() => cuts.map((cut) => {
    const asset = assets.data?.get(cut.assetId);
    const searchable = [describe(asset), asset?.labels.join(" ") ?? "", asset?.filename ?? ""].join(" ").toLowerCase();
    return { cut, description: describe(asset), filename: asset?.filename ?? "", searchable };
  }), [assets.data, cuts]);

  const visible = query
    ? shots.filter((shot) => shot.searchable.includes(query))
    : shots;

  return (
    <section className={styles.section} aria-labelledby="shots-title">
      <header className={styles.header}>
        <h2 id="shots-title">Shots <span>{query ? `${visible.length} of ${cuts.length}` : cuts.length.toLocaleString()}</span></h2>
        <div className={styles.filter}>
          <Icon name="search" size={16} />
          <input type="search" value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="Filter by what’s in the shot" aria-label="Filter shots" />
        </div>
      </header>
      {visible.length ? (
        <ol className={styles.grid}>
          {visible.map(({ cut, description, filename }) => (
            <li key={cut.operationId} className={styles.shot}>
              <Link to={paths.clip(projectId, cut.assetId)} className={styles.link} title={filename || undefined}>
                <span className={styles.thumb} aria-hidden="true">
                  <span className={styles.number}>{cut.position + 1}</span>
                  <Thumbnail key={client.projectAssetThumbnailUrl(projectId, cut.assetId)} src={client.projectAssetThumbnailUrl(projectId, cut.assetId)} />
                  <span className={styles.index}>{cut.position + 1}</span>
                  <span className={styles.at}>{formatClock(cut.movieInSeconds)}</span>
                </span>
                <span className={styles.meta}>
                  <span className="visually-hidden">Shot {cut.position + 1}, at {formatClock(cut.movieInSeconds)}, </span>
                  <strong>{formatLength(cut.movieOutSeconds - cut.movieInSeconds)}</strong>
                  <span className={styles.description}>{description || (assets.isPending ? "…" : "")}</span>
                </span>
              </Link>
            </li>
          ))}
        </ol>
      ) : <p className={styles.none}>No shots match “{filter}”.</p>}
    </section>
  );
}

/** The clip thumbnail, or nothing (leaving the shot number) when the server has none. */
function Thumbnail({ src }: { src: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) return null;
  return <img src={src} alt="" loading="lazy" decoding="async" onError={() => setFailed(true)} />;
}
