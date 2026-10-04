package com.ospab.overnet

/** The overnet gateway in this process (Rust, `overnet-android`). */
object Gateway {
    init {
        System.loadLibrary("overnet_android")
    }

    /** Starts the gateway once; its SOCKS5 port on 127.0.0.1, or -1. */
    @JvmStatic external fun start(): Int

    /** `{"running", "relays", "exits", "token", "error"}` as JSON. */
    @JvmStatic external fun status(): String

    /** New circuits for everything opened from now on. */
    @JvmStatic external fun newCircuits()
}
