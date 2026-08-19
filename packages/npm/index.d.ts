import type { ChildProcess } from 'node:child_process';

export interface StartOptions {
    /** Path to a baked state image to boot from, instead of an empty catalog. */
    image?: string;
    /** Database to create at startup. Defaults to `test`. */
    database?: string;
    /** TCP port. 0, the default, asks the OS for a free one. */
    port?: number;
    /** Listen on a unix socket: `true` for a temporary path, or a path of your own. */
    socket?: boolean | string;
    /** Require this password. Omitted, any credentials are accepted. */
    password?: string;
    /** Version string reported to clients. */
    serverVersion?: string;
    /** Log every connection, command and result to stderr. */
    trace?: boolean;
    /** Use this binary rather than the resolved one. */
    binary?: string;
    /** How long to wait for the server to announce itself. Defaults to 10s. */
    readyTimeoutMs?: number;
}

export declare class Server {
    readonly child: ChildProcess;
    readonly pid: number | undefined;
    readonly port: number;
    readonly socketPath: string | null;
    /** What the server reported for its own startup, in milliseconds. */
    readonly startupMs: number;
    readonly database: string;
    readonly password: string | undefined;
    /** Resolves when the process exits. */
    readonly exited: Promise<unknown>;

    /** A connection URL, over the socket where there is one. */
    readonly url: string;

    /** Stops the server and waits for it to go. Safe to call twice. */
    stop(options?: { timeoutMs?: number }): Promise<void>;
}

/** Starts a server and resolves once it is accepting connections. */
export declare function start(options?: StartOptions): Promise<Server>;

/** The platform key for this machine, e.g. `linux-x64-musl`. */
export declare function platformKey(): string;

/** Platform key to the package that carries that binary. */
export declare const PLATFORMS: Record<string, string>;

/** Where the binary is, or an error explaining why it is not. */
export declare function resolveBinary(override?: string): string;
