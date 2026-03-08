export interface NonceEntry {
  nonce: string
  pubkey: string
  username: string
  expiresAt: number
}

const TTL_MS = 5 * 60 * 1000 // 5 minutes

export class NonceStore {
  private store = new Map<string, NonceEntry>()

  set(nonce: string, pubkey: string, username: string): void {
    this.cleanup()
    this.store.set(nonce, {
      nonce,
      pubkey,
      username,
      expiresAt: Date.now() + TTL_MS,
    })
  }

  findByPubkey(pubkey: string): NonceEntry | undefined {
    this.cleanup()
    for (const entry of this.store.values()) {
      if (entry.pubkey === pubkey) return entry
    }
    return undefined
  }

  delete(nonce: string): boolean {
    return this.store.delete(nonce)
  }

  private cleanup(): void {
    const now = Date.now()
    for (const [key, entry] of this.store) {
      if (entry.expiresAt <= now) {
        this.store.delete(key)
      }
    }
  }
}

export const nonceStore = new NonceStore()
