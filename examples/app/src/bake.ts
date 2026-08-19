#!/usr/bin/env node
// `npm run bake`: apply the migrations once and snapshot the result.
//
// Every runner example calls this from `pretest`, so the expensive step happens
// in one process before any test worker exists.

import { bake, imagePath } from './testing';

bake()
    .then(() => console.log(`image ready: ${imagePath()}`))
    .catch((e: unknown) => {
        console.error(e instanceof Error ? e.message : e);
        process.exit(1);
    });
