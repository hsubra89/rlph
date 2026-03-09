import { HashMap } from "effect";

/** Remove all map entries for which `isExpired` returns true. */
export const pruneExpired = <K, V>(
  map: HashMap.HashMap<K, V>,
  isExpired: (value: V) => boolean,
): HashMap.HashMap<K, V> => {
  let pruned = map;
  for (const [k, v] of map) {
    if (isExpired(v)) pruned = HashMap.remove(pruned, k);
  }
  return pruned;
};
