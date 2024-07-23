# Substrate handshake

## Overview

This project is a Rust-based application designed to facilitate secure communication with a Substrate blockchain node. The application performs a series of steps to ensure the integrity and authenticity of the connection between the client and the node. These steps include:

Establishing a Connection: The application connects to a Substrate node via WebSocket, a protocol that enables two-way communication between the client and the server.

Performing a Handshake: A handshake is initiated to authenticate the connection. This process involves sending a structured handshake message to the node and waiting for a corresponding response. The handshake ensures that both the client and the node agree on the communication protocol and trust each other.

Querying Node Information: After the handshake is successfully completed, the application sends JSON-RPC requests to the node to retrieve key information. The queries include requests for the node's name, the blockchain it is part of, and the software version it is running. This information helps verify the identity and status of the node.

Logging: The application provides detailed logging throughout the process. This includes logs for connection status, handshake messages, JSON-RPC requests, and responses. The logging helps in debugging and monitoring the communication with the node.

The primary goal of this project is to ensure that a client can establish a secure and authenticated communication channel with a Substrate node, verify the connection, and retrieve essential node information.

## Features

Handshake with Substrate Node:

Establishes a secure connection and performs a handshake to authenticate the communication channel.
Query Node Information: Retrieves and logs the node's name, chain, and version using JSON-RPC calls.
Logging: Provides detailed logging of each step, including connection status, handshake completion, and JSON-RPC responses.

## Prerequisites

Before running this program, ensure you have the following installed:

Rust and Cargo.
Substrate Node: A local instance of a Substrate node that the program can connect to.

## Installation and Setup

### Step 1: Install Rust and Cargo

Download and Install:

Visit the official Rust website: https://www.rust-lang.org/tools/install
Follow the instructions to download and install Rust and Cargo.
Verify Installation:

### Step 2: Set Up and Run a Local Substrate Node

Install Substrate:

Substrate is a blockchain framework. To install it, use the installation script provided by the Substrate team. This script will install all necessary dependencies and set up Substrate on your system.

Run the following command in your terminal:

curl https://getsubstrate.io -sSf | bash

Clone the Substrate Node Template:

The Substrate Node Template is a basic implementation of a blockchain node using Substrate. You will use this template to set up your local node.
Run the following commands to clone the template and navigate into its directory:

git clone https://github.com/substrate-developer-hub/substrate-node-template

cd substrate-node-template

Build the Node:

Once you have cloned the template, you need to build it. This step compiles the node's code.
Run the following command to build the node:

cargo build --release

Run the Node:

After building the node, you can run it in development mode. This command starts the node and makes it accessible for connections.
Run the following command to start the node:

./target/release/node-template --dev

The node will start, and you should see logs indicating that it is running. Leave this terminal window open and running while you proceed to the next steps.

### Step 3: Clone this project:

git clone https://github.com/emboth/substrate_handshake.git

## Usage

Command-line Arguments:

Node Address: The WebSocket address of the Substrate node. The default value is ws://127.0.0.1:9944.
Genesis Hash: The genesis hash of the blockchain. This is used to verify the identity of the node. The default value is 5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83.

## Run the Program:

Use Cargo to run the program with the necessary arguments:

cargo run -- --node-address ws://127.0.0.1:9944 --genesis-hash 5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83

Example Output:

When the program runs successfully, you should see logs indicating the progress of the connection, handshake, and querying processes. These logs will include messages confirming the connection to the node, the completion of the handshake, and the details of the node (such as its name, chain, and version). Example log messages might include:

INFO substrate_handshake > Connecting to node at ws://127.0.0.1:9944
INFO substrate_handshake > Connected to the node with response: ...
INFO substrate_handshake > Handshake completed!
INFO substrate_handshake > Sending request: {"id":1,"jsonrpc":"2.0","method":"system_name","params":[]}
INFO substrate_handshake > Sending request: {"id":2,"jsonrpc":"2.0","method":"system_chain","params":[]}
INFO substrate_handshake > Sending request: {"id":3,"jsonrpc":"2.0","method":"system_version","params":[]}
INFO substrate_handshake > Received response for request id 1: {"id":1,"jsonrpc":"2.0","result":"Substrate Node"}
INFO substrate_handshake > Received response for request id 2: {"id":2,"jsonrpc":"2.0","result":"Development"}
INFO substrate_handshake > Received response for request id 3: {"id":3,"jsonrpc":"2.0","result":"0.0.0-d70f8f9793c"}
INFO substrate_handshake > Node information queried!

### This README provides all the necessary information to set up, run, and understand the program. It includes an expanded overview of the project, detailed installation instructions, usage details, and command-line arguments.
