#!/bin/bash

# Update pipeline status to running
ginger-connector update-pipeline stage running

# Build the application
if cargo build --release; then
    # Update pipeline status to passing if build succeeds
    ginger-connector update-pipeline stage passing
else
    # Update pipeline status to failed if build fails
    ginger-connector update-pipeline stage failed
    exit 1
fi
